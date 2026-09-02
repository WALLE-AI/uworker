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
use agentrs_contracts::ports::{
    Clock, ContentStore, MemoryFragment, PersistError, RunEventSink, RunPersistence, SkillManifest,
};
use agentrs_types::{LlmEvent, LlmRequest, StopReason, TokenUsage};

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
    /// Provider reported token usage accumulated across all model requests.
    pub usage: TokenUsage,
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

    /// Delivers model events as they become available.
    ///
    /// Drivers without a genuinely streaming transport inherit a compatible
    /// implementation that emits the collected result in order.
    async fn call_stream(
        &self,
        req: LlmRequest,
        emit: Arc<dyn Fn(LlmEvent) + Send + Sync>,
    ) -> Result<(), String> {
        for event in self.call(req).await? {
            emit(event);
        }
        Ok(())
    }
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
    /// 可丢失的实时事件观察面，不参与事实提交。
    pub event_sink: Option<Arc<dyn RunEventSink>>,
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
    /// Optional immutable content sources supplied by Core.
    pub context: Option<ContextDeps>,
    /// Optional Phase D component generation source.
    pub components: Option<ComponentDeps>,
}

/// Components whose generations must be committed for each model operation.
#[derive(Clone)]
pub struct ComponentDeps {
    /// Authoritative generation manager.
    pub manager: Arc<crate::generation::GenerationManager>,
    /// Root components used by this Run; dependencies are included recursively.
    pub roots: Vec<agentrs_contracts::ids::ComponentId>,
}

/// Content inputs resolved before the first model request.
#[derive(Clone, Default)]
pub struct ContextSources {
    /// Additional system sections.
    pub system_sections: Vec<agentrs_contracts::content::ContentRef>,
    /// Selected memory fragments.
    pub memories: Vec<MemoryFragment>,
    /// Enabled skill manifests.
    pub skills: Vec<SkillManifest>,
    /// Reusable compaction summaries.
    pub compaction_refs: Vec<agentrs_contracts::content::ContentRef>,
}

/// ContentStore and its frozen per-run source selection.
#[derive(Clone)]
pub struct ContextDeps {
    /// Host-owned content store.
    pub store: Arc<dyn ContentStore>,
    /// Sources selected by Core for this run.
    pub sources: ContextSources,
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
            event_sink: None,
            clock,
            driver,
            admission,
            tools: None,
            context: None,
            components: None,
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

#[derive(Default)]
struct LlmResponseState {
    text: String,
    /// 本次响应的思考正文。**必须进 Surface**：Anthropic 族要求带签名的
    /// thinking 块原样往返，否则下一次请求 400；而对不接受它的端点，
    /// `legalization` 会在装配时按 provider 能力剥掉——那是记录之后的投影决定，
    /// 与"记没记"是两件事。
    thinking: String,
    /// provider 侧的不透明签名，有生命周期。
    thinking_signature: Option<String>,
    tool_calls: usize,
    stop: Option<StopReason>,
    partial_recorded: bool,
    proposed: Vec<crate::toolround::ProposedCall>,
    usage: TokenUsage,
}

#[derive(Debug, Clone, Default)]
struct PreparedContext {
    system: String,
    messages: Vec<agentrs_types::Message>,
    system_sections: Vec<agentrs_contracts::content::ContentRef>,
    memory_fragments: Vec<agentrs_contracts::ids::MemoryId>,
    skill_fragments: Vec<agentrs_contracts::ids::SkillId>,
    compaction_refs: Vec<agentrs_contracts::content::ContentRef>,
    resolved_content_refs: Vec<agentrs_contracts::content::ContentRef>,
    unresolved: Vec<agentrs_contracts::content::UnresolvedReason>,
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
    live_seq: std::sync::atomic::AtomicU64,
    last_durable_seq: std::sync::atomic::AtomicU64,
    /// 已追加的 Surface 节点。请求**只从这里投影**，
    /// 这样"模型可见即已记录"才是可验证的（见 `invariant`）。
    surface: Mutex<Vec<crate::surface::SurfaceNode>>,
    /// 分叉继承来的前缀，**尚未记入本 Run 的日志**。
    ///
    /// 它在 `RunStarted` 之后被逐条 `record_surface` 写进来，于是新 Run 的日志
    /// 自带完整可重建前缀（fork 规则 3）。只把它塞进 `surface` 是不够的：那样
    /// 模型看得见、日志里却没有，`--resume` 会重建出一段缺了开头的对话，而
    /// "模型可见即已记录"这条不变量正是用来禁止这种状态的。
    inherited: Mutex<Vec<crate::surface::SurfaceNode>>,
    invariants: crate::invariant::Invariants,
    workspace_id: String,
    change_set_id: agentrs_contracts::ids::ChangeSetId,
    approval_timeout_ms: i64,
    model: agentrs_contracts::ids::ModelId,
    system_prompt: String,
    authority_id: agentrs_contracts::ids::AuthorityEnvelopeId,
    capability_digest: agentrs_contracts::authority::CapabilityViewDigest,
    context_budget: agentrs_contracts::spec::ContextBudget,
    spec_version: agentrs_contracts::version::SpecVersion,
    prepared_context: Mutex<Option<PreparedContext>>,
    usage: Mutex<TokenUsage>,
    /// 模型可见的工具目录。**只追加不重排**（缓存前缀 S1 段）。
    tool_catalog: Vec<agentrs_types::ToolDef>,
    /// 未经权限模式投影的完整目录。
    ///
    /// schema 校验与 [`ModeGuard`](crate::permission::ModeGuard) 都要它，理由同一条：
    /// 得能区分"这个工具不存在"与"存在但当前模式不允许"。拿投影后的目录去校验，
    /// Plan 模式下的一次写调用会被报成"没有这个工具"，而正确的答复是
    /// "Write 在只读探索模式下不可用"。
    full_catalog: Vec<agentrs_types::ToolDef>,
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
            live_seq: std::sync::atomic::AtomicU64::new(0),
            last_durable_seq: std::sync::atomic::AtomicU64::new(0),
            surface: Mutex::new(Vec::new()),
            inherited: Mutex::new(Vec::new()),
            invariants: crate::invariant::Invariants::default(),
            workspace_id: "default".into(),
            change_set_id: "cs-default".into(),
            approval_timeout_ms: 60_000,
            model: "default".into(),
            system_prompt: String::new(),
            authority_id: "unset".into(),
            capability_digest: agentrs_contracts::authority::CapabilityViewDigest(
                agentrs_contracts::ids::Digest::from_hex("unset"),
            ),
            context_budget: agentrs_contracts::spec::ContextBudget {
                max_input_tokens: 100_000,
                reserved_output_tokens: 4_000,
                compaction_threshold_pct: 80,
            },
            spec_version: agentrs_contracts::version::SpecVersion(1),
            prepared_context: Mutex::new(None),
            usage: Mutex::new(TokenUsage::default()),
            tool_catalog: Vec::new(),
            full_catalog: Vec::new(),
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

    /// 设置冻结的授权/能力视图和上下文预算，供每次请求 manifest 使用。
    pub fn with_run_context(
        mut self,
        authority_id: agentrs_contracts::ids::AuthorityEnvelopeId,
        capability_digest: agentrs_contracts::authority::CapabilityViewDigest,
        context_budget: agentrs_contracts::spec::ContextBudget,
        spec_version: agentrs_contracts::version::SpecVersion,
    ) -> Self {
        self.authority_id = authority_id;
        self.capability_digest = capability_digest;
        self.context_budget = context_budget;
        self.spec_version = spec_version;
        self
    }

    /// 设置模型可见的工具目录。
    pub fn with_tools(mut self, tools: Vec<agentrs_types::ToolDef>) -> Self {
        self.full_catalog.clone_from(&tools);
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

    /// Restore the durable Surface projection and sequence cursor.
    /// 装入分叉继承的前缀。
    ///
    /// 与 [`Self::with_recovery_state`] 的区别是**它会被写进本 Run 的日志**：
    /// 恢复重放的是同一个 Run 自己的历史，日志里本来就有；分叉来的历史在另一个
    /// Run 的日志里，不搬过来这条日志就不自足。
    pub fn with_inherited_surface(mut self, nodes: Vec<crate::surface::SurfaceNode>) -> Self {
        self.inherited = Mutex::new(nodes);
        self
    }

    /// Restore the durable Surface projection and sequence cursor.
    pub fn with_recovery_state(
        mut self,
        surface: Vec<crate::surface::SurfaceNode>,
        last_durable_seq: agentrs_contracts::ids::EventSequence,
    ) -> Self {
        self.surface = Mutex::new(surface);
        self.last_durable_seq = std::sync::atomic::AtomicU64::new(last_durable_seq.0);
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
        let mut ev = RunEventEnvelope {
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
        if ev.is_durable() {
            let seq = self.deps.persistence.append_event(self.epoch, ev.clone()).await?;
            ev.seq = Some(seq);
            self.last_durable_seq
                .store(seq.0, std::sync::atomic::Ordering::SeqCst);
        } else {
            ev.live_seq = Some(agentrs_contracts::ids::LiveSequence(
                self.live_seq.fetch_add(1, Ordering::SeqCst) + 1,
            ));
        }
        if let Some(sink) = &self.deps.event_sink {
            let _ = sink.publish(ev).await;
        }
        Ok(())
    }

    async fn emit_fact(&self, payload: EventPayload, c: Causality) -> Result<(), PersistError> {
        self.emit(payload, Durability::DurableFact, c).await
    }

    /// 把一条消息追加进 Surface。
    ///
    /// **这是模型可见内容进入系统的唯一入口**——请求装配只从 Surface 投影，
    /// 因此绕过这里的内容会被运行时不变式捕获。
    async fn record_surface(
        &self,
        kind: agentrs_contracts::surface::SurfaceEventKind,
        op: agentrs_contracts::surface::SurfaceOp,
        message: agentrs_types::Message,
        causality: Causality,
    ) -> Result<(), PersistError> {
        self.record_surface_inner(kind, op, message, causality, true).await
    }

    /// Record a Surface node without announcing it on the live channel.
    ///
    /// 只有分叉继承的前缀走这条路。它必须落进日志（fork 规则 3：新 Run 自带
    /// 完整可重建前缀），但**不该再广播一次**——那段历史宿主早就见过并且正显示
    /// 在屏幕上，再推一遍就是把同一段对话贴第二份。恢复也是这么做的：
    /// `resume_from_events` 重建 Surface 时同样不重播事件。
    async fn record_inherited_surface(
        &self,
        kind: agentrs_contracts::surface::SurfaceEventKind,
        op: agentrs_contracts::surface::SurfaceOp,
        message: agentrs_types::Message,
        causality: Causality,
    ) -> Result<(), PersistError> {
        self.record_surface_inner(kind, op, message, causality, false).await
    }

    async fn record_surface_inner(
        &self,
        kind: agentrs_contracts::surface::SurfaceEventKind,
        op: agentrs_contracts::surface::SurfaceOp,
        message: agentrs_types::Message,
        causality: Causality,
        publish: bool,
    ) -> Result<(), PersistError> {
        let mut event = RunEventEnvelope {
            run_id: self.run_id.clone(),
            epoch: self.epoch,
            event_id: self.next_event_id(),
            seq: None,
            live_seq: None,
            at: self.deps.clock.now(),
            durability: Durability::DurableFact,
            visibility: Visibility::User,
            causality,
            surface: Some(agentrs_contracts::surface::SurfaceMarker { kind, op }),
            payload: EventPayload::SurfaceMessageRecorded {
                message: serde_json::to_value(&message).expect("Message is serializable"),
            },
        };
        let seq = self
            .deps
            .persistence
            .append_event(self.epoch, event.clone())
            .await?;
        event.seq = Some(seq);
        if publish {
            if let Some(sink) = &self.deps.event_sink {
                let _ = sink.publish(event).await;
            }
        }
        self.last_durable_seq
            .store(seq.0, std::sync::atomic::Ordering::SeqCst);
        let mut s = self.surface.lock().await;
        s.push(crate::surface::SurfaceNode {
            seq,
            kind,
            op,
            message,
        });
        Ok(())
    }

    /// 从 Surface 投影出即将发送的历史。
    async fn project_history(&self) -> Vec<agentrs_types::Message> {
        crate::surface::derive_messages(&self.surface.lock().await)
    }

    async fn resolve_context(&self) -> Result<PreparedContext, agentrs_context::AssembleError> {
        let Some(context) = &self.deps.context else {
            return Ok(PreparedContext {
                system: self.system_prompt.clone(),
                ..Default::default()
            });
        };
        let mut prepared = PreparedContext {
            system: self.system_prompt.clone(),
            ..Default::default()
        };

        let system =
            agentrs_context::resolve_text_refs(context.store.as_ref(), &context.sources.system_sections)
                .await?;
        if !system.texts.is_empty() {
            if !prepared.system.is_empty() {
                prepared.system.push_str("\n\n");
            }
            prepared.system.push_str(&system.texts.join("\n\n"));
        }
        prepared.system_sections = system.resolved_refs.clone();
        prepared.resolved_content_refs.extend(system.resolved_refs);
        prepared.unresolved.extend(system.unresolved);

        for memory in &context.sources.memories {
            let resolution = agentrs_context::resolve_text_refs(
                context.store.as_ref(),
                std::slice::from_ref(&memory.content),
            )
            .await?;
            if !resolution.texts.is_empty() {
                prepared.memory_fragments.push(memory.id.clone());
            }
            for text in resolution.texts {
                prepared.messages.push(agentrs_types::Message::new(
                    agentrs_types::Role::User,
                    vec![agentrs_types::ContentBlock::text(format!(
                        "[memory:{}]\n{text}",
                        memory.id
                    ))],
                ));
            }
            prepared.resolved_content_refs.extend(resolution.resolved_refs);
            prepared.unresolved.extend(resolution.unresolved);
        }

        for skill in &context.sources.skills {
            let resolution = agentrs_context::resolve_text_refs(
                context.store.as_ref(),
                std::slice::from_ref(&skill.content),
            )
            .await?;
            if !resolution.texts.is_empty() {
                prepared.skill_fragments.push(skill.id.clone());
            }
            for text in resolution.texts {
                prepared.messages.push(agentrs_types::Message::new(
                    agentrs_types::Role::User,
                    vec![agentrs_types::ContentBlock::text(format!(
                        "[skill:{}@{}]\n{text}",
                        skill.id, skill.version
                    ))],
                ));
            }
            prepared.resolved_content_refs.extend(resolution.resolved_refs);
            prepared.unresolved.extend(resolution.unresolved);
        }

        for reference in &context.sources.compaction_refs {
            let resolution =
                agentrs_context::resolve_text_refs(context.store.as_ref(), std::slice::from_ref(reference))
                    .await?;
            for text in resolution.texts {
                prepared.messages.push(agentrs_types::Message::new(
                    agentrs_types::Role::User,
                    vec![agentrs_types::ContentBlock::text(format!("[compaction]\n{text}"))],
                ));
            }
            prepared
                .compaction_refs
                .extend(resolution.resolved_refs.iter().cloned());
            prepared.resolved_content_refs.extend(resolution.resolved_refs);
            prepared.unresolved.extend(resolution.unresolved);
        }
        Ok(prepared)
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

        // 继承来的前缀先落进本 Run 的日志，再开始第一个 Turn。顺序是重点：
        // 它必须在任何模型请求之前完成，否则"模型可见即已记录"会有一个窗口不成立。
        let inherited = std::mem::take(&mut *self.inherited.lock().await);
        for node in inherited {
            if let Err(e) = self
                .record_inherited_surface(node.kind, node.op, node.message, Causality::default())
                .await
            {
                return self.abort(e, st).await;
            }
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
                self.record_surface(
                    agentrs_contracts::surface::SurfaceEventKind::UserMessage,
                    agentrs_contracts::surface::SurfaceOp::Append,
                    msg,
                    step_c.clone(),
                )
                .await?;
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
        let Some(components) = &self.deps.components else {
            return self.execute_step_with_generations(c, Default::default()).await;
        };
        let scope = c
            .operation_id
            .as_ref()
            .map(|id| format!("operation:{}", id.as_str()))
            .or_else(|| {
                c.step_id
                    .as_ref()
                    .map(|id| format!("operation:step:{}", id.as_str()))
            })
            .unwrap_or_else(|| "operation:unscoped".into());
        let operation_owner = match self.owner.child(scope).await {
            Ok(owner) => owner,
            Err(_) => {
                self.emit_fact(EventPayload::RunFailed, c.clone()).await?;
                return Ok(StepOutcome::EmptyFinal);
            }
        };
        let view = match components
            .manager
            .begin_operation(&components.roots, &operation_owner)
            .await
        {
            Ok(view) => view,
            Err(_) => {
                operation_owner.shutdown().await;
                self.emit_fact(EventPayload::RunFailed, c.clone()).await?;
                return Ok(StepOutcome::EmptyFinal);
            }
        };
        let result = self
            .execute_step_with_generations(c, view.generations().clone())
            .await;
        operation_owner.shutdown().await;
        result
    }

    async fn execute_step_with_generations(
        &self,
        c: &Causality,
        component_generations: std::collections::BTreeMap<
            agentrs_contracts::ids::ComponentId,
            agentrs_contracts::component::Generation,
        >,
    ) -> Result<StepOutcome, PersistError> {
        let request_id: agentrs_contracts::ids::RequestId = format!("{}-req", self.run_id).as_str().into();
        let cached_context = { self.prepared_context.lock().await.clone() };
        let prepared_context = match cached_context {
            Some(prepared) => prepared,
            None => {
                let prepared = match self.resolve_context().await {
                    Ok(prepared) => prepared,
                    Err(_) => {
                        self.emit_fact(EventPayload::RunFailed, c.clone()).await?;
                        return Ok(StepOutcome::EmptyFinal);
                    }
                };
                self.emit_fact(EventPayload::ContextSelected, c.clone()).await?;
                if !prepared.resolved_content_refs.is_empty() {
                    self.emit_fact(
                        EventPayload::ContextContentAttached {
                            refs: prepared.resolved_content_refs.clone(),
                        },
                        c.clone(),
                    )
                    .await?;
                }
                for _ in &prepared.unresolved {
                    self.emit_fact(EventPayload::ContentRefUnresolved, c.clone())
                        .await?;
                }
                let context_already_recorded =
                    self.surface.lock().await.iter().any(|node| {
                        node.kind == agentrs_contracts::surface::SurfaceEventKind::ContextAttached
                    });
                if !context_already_recorded {
                    for message in &prepared.messages {
                        self.record_surface(
                            agentrs_contracts::surface::SurfaceEventKind::ContextAttached,
                            agentrs_contracts::surface::SurfaceOp::Append,
                            message.clone(),
                            c.clone(),
                        )
                        .await?;
                    }
                }
                *self.prepared_context.lock().await = Some(prepared.clone());
                prepared
            }
        };
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
        let snapshot = self.cache_snapshot(&history, &component_generations).await;
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

        let planned_messages = history
            .iter()
            .enumerate()
            .map(|(index, message)| agentrs_context::PlannedMessage {
                label: format!("surface-{index}"),
                priority: if index + 1 == history.len() {
                    agentrs_context::budget::Priority::CurrentInput
                } else if index + 8 >= history.len() {
                    agentrs_context::budget::Priority::RecentHistory
                } else {
                    agentrs_context::budget::Priority::OldHistory
                },
                message: message.clone(),
            })
            .collect();
        let source_end = self
            .last_durable_seq
            .load(std::sync::atomic::Ordering::SeqCst)
            .saturating_add(1);
        let plan = match agentrs_context::assemble(
            agentrs_context::ContextPlanInput {
                request_id: request_id.clone(),
                model: self.model.clone(),
                operation_view: agentrs_contracts::manifest::OperationView {
                    authority_id: self.authority_id.clone(),
                    permission_mode: self.permission_mode.clone(),
                    capability_digest: self.capability_digest.clone(),
                    component_generations,
                },
                source_event_range: agentrs_contracts::ids::EventRange {
                    start: agentrs_contracts::ids::EventSequence(1),
                    end: agentrs_contracts::ids::EventSequence(source_end),
                },
                system: prepared_context.system.clone(),
                system_sections: prepared_context.system_sections.clone(),
                messages: planned_messages,
                tools: self.tool_catalog.clone(),
                memory_fragments: prepared_context.memory_fragments.clone(),
                skill_fragments: prepared_context.skill_fragments.clone(),
                compaction_refs: prepared_context.compaction_refs.clone(),
                resolved_content_refs: prepared_context.resolved_content_refs.clone(),
                surface_digest: snapshot
                    .stable
                    .iter()
                    .find(|segment| segment.kind == agentrs_contracts::manifest::CacheSegment::S2Surface)
                    .map(|segment| segment.digest.clone())
                    .unwrap_or_else(|| agentrs_contracts::ids::Digest::from_hex("surface-empty")),
                surface_invalidation: snapshot.surface_invalidation,
                legalization_ops: Vec::new(),
                unresolved: prepared_context.unresolved.clone(),
                max_input_tokens: self.context_budget.max_input_tokens,
                reserved_output_tokens: self.context_budget.reserved_output_tokens,
            },
            None,
        )
        .await
        {
            Ok(plan) => plan,
            Err(_) => {
                self.emit_fact(EventPayload::RunFailed, c.clone()).await?;
                return Ok(StepOutcome::EmptyFinal);
            }
        };

        self.emit_fact(
            EventPayload::ModelRequestManifestRecorded {
                manifest: Box::new(plan.manifest.clone()),
            },
            c.clone(),
        )
        .await?;
        let up_to_seq = agentrs_contracts::ids::EventSequence(
            self.last_durable_seq.load(std::sync::atomic::Ordering::SeqCst),
        );
        if let Some(context) = &self.deps.context {
            let owner = agentrs_contracts::content::RetentionOwner::Checkpoint {
                run_id: self.run_id.clone(),
                up_to_seq,
            };
            if agentrs_context::retain_manifest_refs(context.store.as_ref(), owner, &plan.manifest)
                .await
                .is_err()
            {
                self.emit_fact(EventPayload::RunFailed, c.clone()).await?;
                return Ok(StepOutcome::EmptyFinal);
            }
        }
        self.deps
            .persistence
            .save_checkpoint(
                self.epoch,
                agentrs_contracts::spec::RunCheckpoint {
                    spec_version: self.spec_version,
                    up_to_seq,
                    pending_approval: None,
                },
            )
            .await?;
        self.emit_fact(EventPayload::Checkpointed, c.clone()).await?;
        self.emit_fact(
            EventPayload::ModelRequestPrepared {
                request_id: request_id.clone(),
            },
            c.clone(),
        )
        .await?;

        let req = LlmRequest {
            request_id,
            model: self.model.clone(),
            system: plan.system,
            messages: plan.messages,
            tools: plan.tools,
            max_tokens: Some(self.context_budget.reserved_output_tokens.min(u32::MAX as u64) as u32),
            thinking: None,
            reasoning_effort: None,
            cache_prefix_digest: Some(plan.manifest.cache_prefix_digest),
        };

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let emit: Arc<dyn Fn(LlmEvent) + Send + Sync> = Arc::new(move |event| {
            let _ = event_tx.send(event);
        });
        let driver = self.deps.driver.clone();
        let call = driver.call_stream(req, emit);
        tokio::pin!(call);
        let mut response = LlmResponseState::default();
        let call_result = loop {
            tokio::select! {
                result = &mut call => break result,
                Some(event) = event_rx.recv() => {
                    self.consume_llm_event(event, &mut response, c).await?;
                }
            }
        };
        while let Ok(event) = event_rx.try_recv() {
            self.consume_llm_event(event, &mut response, c).await?;
        }
        if call_result.is_err() {
            self.emit_fact(EventPayload::RunFailed, c.clone()).await?;
            return Ok(StepOutcome::EmptyFinal);
        }

        let LlmResponseState {
            text,
            thinking,
            thinking_signature,
            tool_calls,
            stop,
            proposed,
            usage: request_usage,
            ..
        } = response;

        {
            let mut total = self.usage.lock().await;
            total.input_tokens = total.input_tokens.saturating_add(request_usage.input_tokens);
            total.output_tokens = total.output_tokens.saturating_add(request_usage.output_tokens);
            total.cache_creation_tokens = total
                .cache_creation_tokens
                .saturating_add(request_usage.cache_creation_tokens);
            total.cache_read_tokens = total
                .cache_read_tokens
                .saturating_add(request_usage.cache_read_tokens);
        }

        if !text.is_empty() || !proposed.is_empty() || !thinking.is_empty() {
            let mut blocks: Vec<agentrs_types::ContentBlock> = Vec::new();
            // 思考在前。这不是审美：Anthropic 族要求 thinking 块位于同一条助手
            // 消息的最前面，顺序错了直接 400。
            if !thinking.is_empty() {
                blocks.push(agentrs_types::ContentBlock::Thinking {
                    thinking: thinking.clone(),
                    signature: thinking_signature.clone(),
                });
            }
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
            self.record_surface(
                agentrs_contracts::surface::SurfaceEventKind::AssistantMessage,
                agentrs_contracts::surface::SurfaceOp::Append,
                agentrs_types::Message::new(agentrs_types::Role::Assistant, blocks),
                c.clone(),
            )
            .await?;
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

    async fn consume_llm_event(
        &self,
        event: LlmEvent,
        response: &mut LlmResponseState,
        causality: &Causality,
    ) -> Result<(), PersistError> {
        match event {
            LlmEvent::TextDelta(text) => {
                // The durable marker is committed before the first live delta.
                if !response.partial_recorded && !text.is_empty() {
                    response.partial_recorded = true;
                    self.emit_fact(EventPayload::PartialOutputStarted, causality.clone())
                        .await?;
                }
                response.text.push_str(&text);
                self.emit(
                    EventPayload::TextDelta { text },
                    Durability::LiveStream,
                    causality.clone(),
                )
                .await?;
            }
            LlmEvent::ToolUse { id, name, input, .. } => {
                response.tool_calls += 1;
                response.proposed.push(crate::toolround::ProposedCall {
                    call_id: id,
                    tool_name: name,
                    arguments: input,
                });
            }
            LlmEvent::ThinkingDelta(text) => {
                response.thinking.push_str(&text);
                self.emit(
                    EventPayload::ThinkingDelta { text },
                    Durability::LiveStream,
                    causality.clone(),
                )
                .await?;
            }
            LlmEvent::ThinkingSignature(signature) => {
                response.thinking_signature = Some(signature);
            }
            LlmEvent::Usage(usage) => response.usage = usage,
            LlmEvent::Done { stop_reason, usage } => {
                response.stop = Some(stop_reason);
                if usage.input_tokens > 0 || usage.output_tokens > 0 {
                    response.usage = usage;
                }
            }
            _ => {}
        }
        Ok(())
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
        // 目录由引擎交，而不是沿用宿主装配时给的那一份：schema 校验必须对着
        // **模型实际被展示过的**那份目录进行。
        let mut guards = host_tools.guards.clone();
        if let Some(g) = &self.mode_guard {
            guards.push(g.clone());
        }
        let tools = Arc::new(crate::toolround::ToolRoundDeps {
            policy: host_tools.policy.clone(),
            sandbox: host_tools.sandbox.clone(),
            guards,
            hooks: host_tools.hooks.clone(),
            catalog: self.full_catalog.clone(),
        });

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
                        self.record_surface(
                            agentrs_contracts::surface::SurfaceEventKind::ToolResult,
                            agentrs_contracts::surface::SurfaceOp::Append,
                            agentrs_types::Message::new(
                                agentrs_types::Role::User,
                                vec![agentrs_types::ContentBlock::ToolResult {
                                    tool_use_id: calls[i].call_id.clone(),
                                    content: text,
                                    is_error,
                                }],
                            ),
                            c.clone(),
                        )
                        .await?;
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
            let s = self.surface.lock().await;
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

            next
        };
        self.record_surface(
            agentrs_contracts::surface::SurfaceEventKind::AssistantMessage,
            SurfaceOp::Replace {
                range: plan.range,
                generation,
            },
            summary,
            c.clone(),
        )
        .await?;

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
        component_generations: &std::collections::BTreeMap<
            agentrs_contracts::ids::ComponentId,
            agentrs_contracts::component::Generation,
        >,
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
            component_generations: component_generations.clone(),
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
        let usage = *self.usage.lock().await;

        // 终态后拒绝新输入，并结算全部 live 资源。
        self.inbox.mark_terminal();
        let _ = self.owner.shutdown().await;

        RunSummary {
            termination,
            turns: st.turns,
            steps: st.steps,
            zero_step_turns: st.zero_step_turns,
            usage,
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
        let usage = *self.usage.lock().await;

        RunSummary {
            termination: Termination::Failed { code: code.into() },
            turns: st.turns,
            steps: st.steps,
            zero_step_turns: st.zero_step_turns,
            usage,
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

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<agentrs_contracts::event::RunEventEnvelope>>,
    }

    #[async_trait::async_trait]
    impl RunEventSink for RecordingSink {
        async fn publish(
            &self,
            event: agentrs_contracts::event::RunEventEnvelope,
        ) -> Result<(), agentrs_contracts::ports::EventSinkError> {
            self.events.lock().unwrap().push(event);
            Ok(())
        }
    }

    fn 装配(script: Vec<Vec<LlmEvent>>, admission: Arc<dyn StepAdmission>, guards: TurnGuards) -> Fixture {
        装配带_sink(script, admission, guards, None)
    }

    fn 装配带_sink(
        script: Vec<Vec<LlmEvent>>,
        admission: Arc<dyn StepAdmission>,
        guards: TurnGuards,
        event_sink: Option<Arc<dyn RunEventSink>>,
    ) -> Fixture {
        let persistence = Arc::new(agentrs_testkit::FakePersistence::new());
        let inbox = Arc::new(Inbox::new(16));
        let cancel = Arc::new(CancelToken::default());
        let owner = ResourceOwner::new("run");
        let engine = Engine::new(
            "r1".into(),
            RunEpoch(1),
            EngineDeps {
                persistence: persistence.clone(),
                event_sink,
                clock: Arc::new(FixedClock(Timestamp(0))),
                driver: ScriptedDriver::new(script),
                admission,
                tools: None,
                context: None,
                components: None,
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
    async fn manifest_先于模型请求边界持久化() {
        let f = 装配(vec![文本_final("hi")], Arc::new(AdmitAll), TurnGuards::default());
        f.engine.run().await;

        let events = f.persistence.events();
        let manifest = events
            .iter()
            .position(|event| matches!(event.payload, EventPayload::ModelRequestManifestRecorded { .. }))
            .expect("必须记录 manifest");
        let prepared = events
            .iter()
            .position(|event| matches!(event.payload, EventPayload::ModelRequestPrepared { .. }))
            .expect("必须记录请求边界");
        assert!(manifest < prepared, "恢复不能先看到请求边界、后看到构成清单");

        let manifest_id = match &events[manifest].payload {
            EventPayload::ModelRequestManifestRecorded { manifest } => &manifest.request_id,
            _ => unreachable!(),
        };
        let request_id = match &events[prepared].payload {
            EventPayload::ModelRequestPrepared { request_id } => request_id,
            _ => unreachable!(),
        };
        assert_eq!(manifest_id, request_id);
    }

    #[tokio::test]
    async fn manifest_记录本_operation_固定的_component_generations() {
        use crate::generation::{
            CandidateError, CandidateFactory, ComponentCandidate, CompositionProfile, GenerationManager,
        };
        use agentrs_contracts::authority::CapabilityView;
        use agentrs_contracts::component::{
            ComponentKind, ComponentManifest, ComponentScope, ComponentTrust,
        };

        struct Ready;
        #[async_trait::async_trait]
        impl CandidateFactory for Ready {
            async fn prepare(
                &self,
                _candidate: &ComponentCandidate,
                _generation: agentrs_contracts::component::Generation,
                _owner: Arc<ResourceOwner>,
            ) -> Result<(), CandidateError> {
                Ok(())
            }
        }

        let capabilities = CapabilityView {
            tools: vec![],
            providers: vec!["provider".into()],
            models: vec!["model".into()],
        };
        let manager = GenerationManager::new(ResourceOwner::new("composition"));
        manager
            .apply_profile(
                CompositionProfile {
                    revision: 1,
                    components: vec![ComponentCandidate {
                        manifest: ComponentManifest {
                            id: "provider.main".into(),
                            source: "builtin:provider".into(),
                            version: "1".into(),
                            api_version: 1,
                            kind: ComponentKind::Provider,
                            requires: vec![],
                            provides: vec!["provider".into()],
                            config_schema: serde_json::json!({"type":"object"}),
                            scope: ComponentScope::Run,
                            trust: ComponentTrust::TrustedBuiltin,
                            requested_capabilities: capabilities.clone(),
                            redacted_config_fields: vec![],
                        },
                        config: serde_json::json!({}),
                    }],
                },
                &capabilities,
                &Ready,
            )
            .await
            .unwrap();

        let mut fixture = 装配(vec![文本_final("hi")], Arc::new(AdmitAll), TurnGuards::default());
        fixture.engine.deps.components = Some(ComponentDeps {
            manager: manager.clone(),
            roots: vec!["provider.main".into()],
        });
        fixture.engine.run().await;
        let generations = fixture
            .persistence
            .events()
            .into_iter()
            .find_map(|event| match event.payload {
                EventPayload::ModelRequestManifestRecorded { manifest } => {
                    Some(manifest.operation_view.component_generations)
                }
                _ => None,
            })
            .expect("manifest recorded");
        assert_eq!(
            generations[&agentrs_contracts::ids::ComponentId::new("provider.main")],
            agentrs_contracts::component::Generation(1)
        );
        assert_eq!(manager.inventory().await.entries[0].in_flight, 0);
    }

    #[tokio::test]
    async fn content_refs_进入请求并在_checkpoint_前被_retain() {
        use agentrs_contracts::content::{ContentMeta, ContentScope};
        use agentrs_contracts::ports::ContentStore;
        use bytes::Bytes;

        struct CaptureRequest(Mutex<Option<LlmRequest>>);
        #[async_trait::async_trait]
        impl StepDriver for CaptureRequest {
            async fn call(&self, req: LlmRequest) -> Result<Vec<LlmEvent>, String> {
                *self.0.lock().unwrap() = Some(req);
                Ok(文本_final("done"))
            }
        }

        let store = Arc::new(agentrs_testkit::FakeContentStore::new());
        async fn put(
            store: &agentrs_testkit::FakeContentStore,
            text: &'static [u8],
        ) -> agentrs_contracts::content::ContentRef {
            store
                .put(
                    ContentScope::Run {
                        run_id: "r-content".into(),
                    },
                    Bytes::from_static(text),
                    ContentMeta::default(),
                )
                .await
                .unwrap()
        }
        let system = put(&store, b"system-ref").await;
        let memory = put(&store, b"memory-ref").await;
        let skill = put(&store, b"skill-ref").await;
        let compact = put(&store, b"compact-ref").await;
        let persistence = Arc::new(agentrs_testkit::FakePersistence::new());
        let captured = Arc::new(CaptureRequest(Mutex::new(None)));
        let inbox = Arc::new(Inbox::new(8));
        inbox
            .submit(UserInput::Message(vec![agentrs_types::ContentBlock::text("go")]))
            .await
            .unwrap();
        let engine = Engine::new(
            "r-content".into(),
            RunEpoch(1),
            EngineDeps {
                persistence: persistence.clone(),
                event_sink: None,
                clock: Arc::new(FixedClock(Timestamp(0))),
                driver: captured.clone(),
                admission: Arc::new(AdmitAll),
                tools: None,
                context: Some(ContextDeps {
                    store: store.clone(),
                    sources: ContextSources {
                        system_sections: vec![system],
                        memories: vec![MemoryFragment {
                            id: "mem-1".into(),
                            content: memory,
                        }],
                        skills: vec![SkillManifest {
                            id: "skill-1".into(),
                            version: "1".into(),
                            content: skill,
                            tool_subset: None,
                        }],
                        compaction_refs: vec![compact],
                    },
                }),
                components: None,
            },
            inbox,
            Arc::new(CancelToken::default()),
            ResourceOwner::new("content-run"),
            TurnGuards::default(),
        )
        .with_model("model", "base-system")
        .with_run_context(
            "authority".into(),
            agentrs_contracts::authority::CapabilityViewDigest(Digest::from_hex("cap")),
            agentrs_contracts::spec::ContextBudget {
                max_input_tokens: 10_000,
                reserved_output_tokens: 100,
                compaction_threshold_pct: 80,
            },
            agentrs_contracts::version::SpecVersion(1),
        );

        let summary = engine.run().await;
        assert_eq!(summary.termination, Termination::Completed);
        let req = captured.0.lock().unwrap().clone().unwrap();
        assert!(req.system.contains("system-ref"));
        let messages = serde_json::to_string(&req.messages).unwrap();
        assert!(messages.contains("memory-ref"));
        assert!(messages.contains("skill-ref"));
        assert!(messages.contains("compact-ref"));

        let manifest = persistence
            .events()
            .into_iter()
            .find_map(|event| match event.payload {
                EventPayload::ModelRequestManifestRecorded { manifest } => Some(manifest),
                _ => None,
            });
        let manifest = manifest.expect("manifest recorded");
        assert_eq!(manifest.resolved_content_refs.len(), 4);
        assert_eq!(
            manifest.memory_fragments,
            [agentrs_contracts::ids::MemoryId::new("mem-1")]
        );
        assert_eq!(
            manifest.skill_fragments,
            [agentrs_contracts::ids::SkillId::new("skill-1")]
        );
        assert!(persistence.checkpoint().is_some());
        assert_eq!(store.collect_unretained(), 0, "checkpoint refs must remain live");
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
        let sink = Arc::new(RecordingSink::default());
        let f = 装配带_sink(
            vec![文本_final("hello")],
            Arc::new(AdmitAll),
            TurnGuards::default(),
            Some(sink.clone()),
        );
        f.engine.run().await;

        assert!(
            f.persistence.events().iter().all(|event| event.is_durable()),
            "Persistence 只能收到 durable 事实"
        );
        let events = sink.events.lock().unwrap();
        let delta = events
            .iter()
            .find(|event| matches!(event.payload, EventPayload::TextDelta { .. }))
            .expect("sink 必须收到文本增量");
        assert_eq!(delta.seq, None);
        assert_eq!(delta.live_seq, Some(agentrs_contracts::ids::LiveSequence(1)));
        assert!(matches!(
            &delta.payload,
            EventPayload::TextDelta { text } if text == "hello"
        ));
        assert!(
            events
                .iter()
                .filter(|event| event.is_durable())
                .all(|event| event.seq.is_some() && event.live_seq.is_none()),
            "sink 中的 durable 事实必须带持久序号"
        );
    }

    #[tokio::test]
    async fn streaming_driver_完成前_live_delta_已到达_sink() {
        struct GatedDriver {
            release: Arc<tokio::sync::Notify>,
        }

        #[async_trait::async_trait]
        impl StepDriver for GatedDriver {
            async fn call(&self, _req: LlmRequest) -> Result<Vec<LlmEvent>, String> {
                unreachable!("engine must use call_stream")
            }

            async fn call_stream(
                &self,
                _req: LlmRequest,
                emit: Arc<dyn Fn(LlmEvent) + Send + Sync>,
            ) -> Result<(), String> {
                emit(LlmEvent::TextDelta("early".into()));
                self.release.notified().await;
                emit(LlmEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: TokenUsage::default(),
                });
                Ok(())
            }
        }

        let sink = Arc::new(RecordingSink::default());
        let mut fixture = 装配带_sink(
            vec![],
            Arc::new(AdmitAll),
            TurnGuards::default(),
            Some(sink.clone()),
        );
        let release = Arc::new(tokio::sync::Notify::new());
        fixture.engine.deps.driver = Arc::new(GatedDriver {
            release: release.clone(),
        });
        let task = tokio::spawn(async move { fixture.engine.run().await });

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if sink.events.lock().unwrap().iter().any(
                    |event| matches!(&event.payload, EventPayload::TextDelta { text } if text == "early"),
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("live delta must not wait for provider completion");
        assert!(!task.is_finished());

        release.notify_one();
        let summary = task.await.unwrap();
        assert_eq!(summary.termination, Termination::Completed);
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
                event_sink: None,
                clock: Arc::new(FixedClock(Timestamp(0))),
                driver: ScriptedDriver::new(vec![文本_final("x")]),
                admission: Arc::new(AdmitAll),
                tools: None,
                context: None,
                components: None,
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
