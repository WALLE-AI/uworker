//! RuntimeHost 与 RunHandle（架构 §4.1，任务 T04）。
//!
//! ## 内核不拥有异步运行时
//!
//! [`RuntimeHost::start`] **不 spawn 任务**，而是返回一个 `driver` future 交给宿主去驱动。
//! 内核是可嵌入的库，不该替宿主决定用什么运行时、在哪个线程池上跑。
//!
//! ## Epoch 是 resume 的核心
//!
//! 每次 `start`/`resume` 分配一个**新的、更大的** [`RunEpoch`]。这一下就把可能仍然
//! 活着的旧 writer 围栏掉了——它后续的任何 durable 写入都会收到 `Fenced` 并被迫收敛。
//!
//! 没有这条，"恢复以 durable log 为准"不成立：进程假死后被重新拉起、旧进程复活继续
//! 写入时，log 已被两个 writer 交错写坏。

use std::collections::HashMap;
use std::sync::Arc;

use agentrs_contracts::event::RunEventEnvelope;
use agentrs_contracts::ids::{EventSequence, RunEpoch, RunId};
use agentrs_contracts::spec::{RunCheckpoint, RunSpec};
use agentrs_contracts::version::{CompatVerdict, CompatWindow, COMPAT_WINDOW};
use futures::future::BoxFuture;
use tokio::sync::Mutex;

use crate::composition::ResourceOwner;
use crate::engine::{CancelToken, Engine, EngineDeps, RunSummary, TurnGuards};
use crate::inbox::{Inbox, InputAccepted, SubmitError, UserInput};

/// 启动失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StartError {
    /// checkpoint 版本低于兼容窗口下界。**拒绝恢复，不静默降级。**
    #[error("checkpoint spec version too old")]
    SpecTooOld,
    /// checkpoint 版本高于当前——降级读取会静默丢字段。
    #[error("checkpoint spec version too new")]
    SpecTooNew,
    /// 同一 Run 已在本 host 上活跃。
    #[error("run already active")]
    AlreadyActive,
    /// `CapabilityView`、模型策略或工作区超出了 Core 签发的授权上界。
    #[error("capability outside authority: {kind}={value}")]
    OutsideAuthority {
        /// 越界的能力种类。
        kind: &'static str,
        /// 越界值。这里只包含稳定标识，不包含用户正文。
        value: String,
    },
    /// 启动所需能力当前不可用。撤回的能力不得靠 fallback 悄悄复活。
    #[error("capability unavailable: {kind}={value}")]
    UnavailableCapability {
        /// 不可用的能力种类。
        kind: &'static str,
        /// 不可用值。
        value: String,
    },
    /// Durable history cannot be replayed deterministically.
    #[error("invalid durable replay: {0}")]
    InvalidReplay(String),
    /// The durable log already contains a terminal event.
    #[error("run is already terminal")]
    AlreadyTerminal,
    /// Recovery must first coordinate with Policy, Sandbox, or the user.
    #[error("recovery requires external action: {0}")]
    RecoveryNeedsAction(&'static str),
}

struct RecoveryState {
    /// True when the surface came from another Run and must be written into
    /// this one's log before anything else happens.
    inherited: bool,
    surface: Vec<crate::surface::SurfaceNode>,
    last_seq: EventSequence,
    epoch_floor: RunEpoch,
}

/// 取消原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelReason {
    /// 用户主动取消。
    User,
    /// 宿主关停。
    HostShutdown,
    /// 预算耗尽。
    BudgetExhausted,
}

/// 一次 Run 的外部句柄。
///
/// 可克隆：inbox 与取消令牌都是 Arc，多个持有者共享同一个 Run。
#[derive(Clone)]
pub struct RunHandle {
    run_id: RunId,
    epoch: RunEpoch,
    inbox: Arc<Inbox>,
    cancel: Arc<CancelToken>,
}

impl RunHandle {
    /// Run 标识。
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    /// 本次驱动分配到的 epoch。
    pub fn epoch(&self) -> RunEpoch {
        self.epoch
    }

    /// 运行中注入输入（steering）。
    ///
    /// **不打断进行中的 Step**——输入进 inbox，在下一个安全边界被 claim。
    /// 若意图是打断，宿主应先 [`RunHandle::cancel`] 再 `submit`：这是两个动作，
    /// UI 层可以把它们组合成一个"打断并改说"按钮。
    pub async fn submit(&self, input: UserInput) -> Result<InputAccepted, SubmitError> {
        self.inbox.submit(input).await
    }

    /// 请求取消。在下一个安全边界收敛。
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Run 是否已进入终态。
    pub fn is_terminal(&self) -> bool {
        self.inbox.is_terminal()
    }
}

/// 启动结果：句柄 + 待驱动的 future。
pub struct StartedRun {
    /// 外部句柄，可在驱动过程中并发使用。
    pub handle: RunHandle,
    /// 驱动 future。**由宿主决定在哪里 spawn。**
    pub driver: BoxFuture<'static, RunSummary>,
}

/// Run 的宿主。
#[derive(Default)]
pub struct RuntimeHost {
    epochs: Mutex<HashMap<RunId, RunEpoch>>,
    compat: Option<CompatWindow>,
}

impl RuntimeHost {
    /// 新建 host。
    pub fn new() -> Self {
        Self::default()
    }

    /// 用自定义兼容窗口新建（测试用）。
    pub fn with_compat(compat: CompatWindow) -> Self {
        Self {
            epochs: Mutex::new(HashMap::new()),
            compat: Some(compat),
        }
    }

    fn window(&self) -> CompatWindow {
        self.compat.unwrap_or(COMPAT_WINDOW)
    }

    /// 为一个 Run 分配下一个 epoch。**单调递增**——这就是围栏。
    async fn next_epoch(&self, run_id: &RunId) -> RunEpoch {
        let mut m = self.epochs.lock().await;
        let next = m.get(run_id).map(|e| e.next()).unwrap_or(RunEpoch(1));
        m.insert(run_id.clone(), next);
        next
    }

    async fn next_epoch_after(&self, run_id: &RunId, floor: RunEpoch) -> RunEpoch {
        let mut epochs = self.epochs.lock().await;
        let current = epochs.get(run_id).copied().unwrap_or(RunEpoch(0));
        let next = RunEpoch(current.0.max(floor.0)).next();
        epochs.insert(run_id.clone(), next);
        next
    }

    /// 启动一次新的 Run。
    pub async fn start(
        &self,
        spec: RunSpec,
        deps: EngineDeps,
        guards: TurnGuards,
    ) -> Result<StartedRun, StartError> {
        self.launch(spec, deps, guards, Vec::new(), None).await
    }

    /// 启动并提供模型可见的工具目录。
    ///
    /// 目录由宿主提供——**内核不持有工具实现，也不自行发现工具**。
    pub async fn start_with_tools(
        &self,
        spec: RunSpec,
        deps: EngineDeps,
        guards: TurnGuards,
        tools: Vec<agentrs_types::ToolDef>,
    ) -> Result<StartedRun, StartError> {
        self.launch(spec, deps, guards, tools, None).await
    }

    /// 从 checkpoint 恢复。
    ///
    /// 先校验版本兼容窗口，再分配**新的** epoch 围栏掉可能仍活着的旧 writer。
    pub async fn resume(
        &self,
        spec: RunSpec,
        checkpoint: RunCheckpoint,
        deps: EngineDeps,
        guards: TurnGuards,
    ) -> Result<StartedRun, StartError> {
        match self.window().verdict(checkpoint.spec_version) {
            CompatVerdict::TooOld => return Err(StartError::SpecTooOld),
            CompatVerdict::TooNew => return Err(StartError::SpecTooNew),
            // NeedsMigration 在此处走显式迁移并写 CheckpointMigrated 事件；
            // W5 只做版本判定，迁移函数随 checkpoint codec 一起落地。
            CompatVerdict::Exact | CompatVerdict::NeedsMigration => {}
        }

        let mut spec = spec;
        spec.checkpoint = Some(checkpoint);
        self.launch(spec, deps, guards, Vec::new(), None).await
    }

    /// Resume from a validated durable event prefix.
    ///
    /// Unsafe boundaries are reported to Core rather than replayed. In particular,
    /// an unresolved execution intent is never converted into a fresh tool call.
    pub async fn resume_from_events(
        &self,
        mut spec: RunSpec,
        checkpoint: RunCheckpoint,
        events: &[RunEventEnvelope],
        deps: EngineDeps,
        guards: TurnGuards,
        tools: Vec<agentrs_types::ToolDef>,
    ) -> Result<StartedRun, StartError> {
        match self.window().verdict(checkpoint.spec_version) {
            CompatVerdict::TooOld => return Err(StartError::SpecTooOld),
            CompatVerdict::TooNew => return Err(StartError::SpecTooNew),
            CompatVerdict::Exact | CompatVerdict::NeedsMigration => {}
        }
        let mut last_seq = EventSequence(0);
        let mut epoch_floor = RunEpoch(0);
        for event in events.iter().filter(|event| event.is_durable()) {
            if event.run_id != spec.run_id {
                return Err(StartError::InvalidReplay("run_id mismatch".into()));
            }
            let seq = event
                .seq
                .ok_or_else(|| StartError::InvalidReplay("durable event missing seq".into()))?;
            if seq <= last_seq {
                return Err(StartError::InvalidReplay(
                    "event sequence is not strictly increasing".into(),
                ));
            }
            last_seq = seq;
            epoch_floor = RunEpoch(epoch_floor.0.max(event.epoch.0));
        }
        if checkpoint.up_to_seq > last_seq {
            return Err(StartError::InvalidReplay(
                "checkpoint points past durable history".into(),
            ));
        }
        match crate::recovery::plan(events) {
            crate::recovery::RecoveryPlan::AlreadyTerminal => return Err(StartError::AlreadyTerminal),
            crate::recovery::RecoveryPlan::ResolvePartialOutput { .. } => {
                return Err(StartError::RecoveryNeedsAction("partial_output"));
            }
            crate::recovery::RecoveryPlan::ReconcileExecution { .. } => {
                return Err(StartError::RecoveryNeedsAction("reconcile_execution"));
            }
            crate::recovery::RecoveryPlan::RedeemApproval { .. } => {
                return Err(StartError::RecoveryNeedsAction("redeem_approval"));
            }
            crate::recovery::RecoveryPlan::ReissueApproval { .. } => {
                return Err(StartError::RecoveryNeedsAction("reissue_approval"));
            }
            crate::recovery::RecoveryPlan::Fresh
            | crate::recovery::RecoveryPlan::RetryModelRequest { .. }
            | crate::recovery::RecoveryPlan::ReuseCompaction { .. } => {}
        }
        let surface = crate::surface::from_events(events)
            .map_err(|_| StartError::InvalidReplay("surface message is malformed".into()))?;
        let has_message_boundary = events.iter().any(|event| {
            matches!(
                event.payload,
                agentrs_contracts::event::EventPayload::UserInputSubmitted
                    | agentrs_contracts::event::EventPayload::AssistantMessage
            )
        });
        if has_message_boundary && surface.is_empty() {
            return Err(StartError::InvalidReplay(
                "message history predates durable surface payloads".into(),
            ));
        }
        spec.checkpoint = Some(checkpoint);
        self.launch(
            spec,
            deps,
            guards,
            tools,
            Some(RecoveryState {
                inherited: false,
                surface,
                last_seq,
                epoch_floor,
            }),
        )
        .await
    }

    /// 启动一个由 [`crate::fork`] 派生的 Run。
    ///
    /// [`crate::fork`] 只产出**规格**：新 `RunId`、收窄后的授权、一个指向源 Run
    /// durable 前缀的 `ConversationSnapshot`。在此之前没有任何公开入口能把它跑
    /// 起来——`start` 会给出空 Surface（等于丢掉整段对话），`resume_from_events`
    /// 又要求 `run_id` 相同且源 Run 未终止。于是分叉一直是导出的死代码。
    ///
    /// 这个入口补上那一步：按边界重放源 Run 的前缀，重建 Surface，再以新 Run
    /// 的身份启动。
    ///
    /// **不查源 Run 的恢复计划**，这是 fork 规则 5 的直接后果：新 Run 不继承任何
    /// live 状态。源 Run 停在一个未决审批上时，那个审批**随源 Run 留在原地**——
    /// 它属于那次已经结束的执行，重新兑现它等于让人为另一件事做过的决定，对这次
    /// 从未发生过的调用生效。
    ///
    /// 三条不变量照旧由 [`crate::fork`] 保证并在此复核：新旧 `RunId` 不同、授权
    /// 不得扩大、`checkpoint` 必须为空。
    pub async fn start_forked(
        &self,
        spec: RunSpec,
        source_events: &[RunEventEnvelope],
        deps: EngineDeps,
        guards: TurnGuards,
        tools: Vec<agentrs_types::ToolDef>,
    ) -> Result<StartedRun, StartError> {
        let Some(source) = spec.conversation.derived_from.clone() else {
            return Err(StartError::InvalidReplay(
                "a forked run must name the run it derives from".into(),
            ));
        };
        if source == spec.run_id {
            return Err(StartError::InvalidReplay(
                "a fork must have a new run id".into(),
            ));
        }
        // 规则 5：live 状态一概不继承，checkpoint 里可能挂着未决审批。
        if spec.checkpoint.is_some() {
            return Err(StartError::InvalidReplay(
                "a forked run must not carry a checkpoint".into(),
            ));
        }
        let boundary = spec.conversation.up_to_seq.unwrap_or(EventSequence(u64::MAX));
        let mut last_seq = EventSequence(0);
        let prefix: Vec<RunEventEnvelope> = source_events
            .iter()
            .filter(|event| event.is_durable())
            .filter(|event| {
                event
                    .seq
                    .is_some_and(|seq| seq <= boundary && seq > EventSequence(0))
            })
            .cloned()
            .collect();
        for event in &prefix {
            if event.run_id != source {
                return Err(StartError::InvalidReplay(
                    "fork prefix mixes events from another run".into(),
                ));
            }
            let seq = event.seq.expect("filtered above");
            if seq <= last_seq {
                return Err(StartError::InvalidReplay(
                    "event sequence is not strictly increasing".into(),
                ));
            }
            last_seq = seq;
        }
        let surface = crate::surface::from_events(&prefix)
            .map_err(|_| StartError::InvalidReplay("surface message is malformed".into()))?;
        if surface.is_empty() {
            return Err(StartError::InvalidReplay(
                "fork prefix carries no conversation to continue".into(),
            ));
        }
        // 新 Run 的日志是空的：它自己的序号从头开始，epoch 也是。源 Run 的序号
        // 不能顺延过来——两条日志各自单调，混用会让恢复读出一段自相矛盾的历史。
        self.launch(
            spec,
            deps,
            guards,
            tools,
            Some(RecoveryState {
                inherited: true,
                surface,
                last_seq: EventSequence(0),
                epoch_floor: RunEpoch(0),
            }),
        )
        .await
    }

    async fn launch(
        &self,
        spec: RunSpec,
        deps: EngineDeps,
        guards: TurnGuards,
        tools: Vec<agentrs_types::ToolDef>,
        recovery: Option<RecoveryState>,
    ) -> Result<StartedRun, StartError> {
        let tools = validate_and_project(&spec, tools)?;
        let run_id = spec.run_id.clone();
        let epoch = match &recovery {
            Some(state) => self.next_epoch_after(&run_id, state.epoch_floor).await,
            None => self.next_epoch(&run_id).await,
        };

        let inbox = Arc::new(Inbox::new(STEERING_CAPACITY));
        let cancel = Arc::new(CancelToken::default());
        let owner = ResourceOwner::new(format!("run:{run_id}"));

        // **`RunSpec` 是唯一配置通道**——引擎的模型、系统段、工作区
        // 全部从它派生，内核不从环境或文件读取任何配置（架构 §1.1）。
        let model = spec
            .model_policy
            .tiers
            .get(&agentrs_contracts::spec::ModelTier::Default)
            .cloned()
            .or_else(|| spec.initial_capabilities.models.first().cloned())
            .ok_or_else(|| StartError::UnavailableCapability {
                kind: "model",
                value: "default-tier".into(),
            })?;

        let system_prompt = spec.system_context.sections.join("\n\n");
        let workspace = spec
            .system_context
            .workspace_id
            .clone()
            .unwrap_or_else(|| "default".to_string());

        let mut engine = Engine::new(
            run_id.clone(),
            epoch,
            deps,
            inbox.clone(),
            cancel.clone(),
            owner,
            guards,
        )
        .with_model(model, system_prompt)
        .with_run_context(
            spec.authority.id.clone(),
            capability_digest(&spec.initial_capabilities),
            spec.context_budget,
            spec.spec_version,
        )
        .with_workspace(workspace, format!("cs-{run_id}"))
        .with_tools(tools)
        // 必须在 with_tools 之后：它要在**完整**目录上做投影。
        .with_permission_mode(spec.permission_mode.clone());
        if let Some(state) = recovery {
            engine = if state.inherited {
                engine.with_inherited_surface(state.surface)
            } else {
                engine.with_recovery_state(state.surface, state.last_seq)
            };
        }

        let handle = RunHandle {
            run_id,
            epoch,
            inbox,
            cancel,
        };

        Ok(StartedRun {
            handle,
            driver: Box::pin(async move { engine.run().await }),
        })
    }

    /// 取消一个已知的 Run。
    ///
    /// 句柄本身也能取消；本方法供只持有 `RunId` 的调用方使用。
    pub async fn known_epoch(&self, run_id: &RunId) -> Option<RunEpoch> {
        self.epochs.lock().await.get(run_id).copied()
    }
}

/// 在创建 live resource、分配 epoch 之前校验 RunSpec，并投影工具目录。
fn validate_and_project(
    spec: &RunSpec,
    tools: Vec<agentrs_types::ToolDef>,
) -> Result<Vec<agentrs_types::ToolDef>, StartError> {
    fn require_subset<T: PartialEq + ToString>(
        current: &[T],
        upper: &[T],
        kind: &'static str,
    ) -> Result<(), StartError> {
        if let Some(value) = current.iter().find(|value| !upper.contains(value)) {
            return Err(StartError::OutsideAuthority {
                kind,
                value: value.to_string(),
            });
        }
        Ok(())
    }

    require_subset(&spec.initial_capabilities.tools, &spec.authority.tools, "tool")?;
    require_subset(
        &spec.initial_capabilities.providers,
        &spec.authority.providers,
        "provider",
    )?;
    require_subset(&spec.initial_capabilities.models, &spec.authority.models, "model")?;

    if let Some(workspace) = &spec.system_context.workspace_id {
        if !spec.authority.workspaces.contains(workspace) {
            return Err(StartError::OutsideAuthority {
                kind: "workspace",
                value: workspace.clone(),
            });
        }
    }

    for provider in &spec.model_policy.providers {
        if !spec.authority.providers.contains(provider) {
            return Err(StartError::OutsideAuthority {
                kind: "provider",
                value: provider.to_string(),
            });
        }
        if !spec.initial_capabilities.providers.contains(provider) {
            return Err(StartError::UnavailableCapability {
                kind: "provider",
                value: provider.to_string(),
            });
        }
    }

    for model in spec
        .model_policy
        .tiers
        .values()
        .chain(spec.model_policy.fallback.iter())
    {
        if !spec.authority.models.contains(model) {
            return Err(StartError::OutsideAuthority {
                kind: "model",
                value: model.to_string(),
            });
        }
        if !spec.initial_capabilities.models.contains(model) {
            return Err(StartError::UnavailableCapability {
                kind: "model",
                value: model.to_string(),
            });
        }
    }

    Ok(tools
        .into_iter()
        .filter(|tool| {
            spec.authority.tools.contains(&tool.name) && spec.initial_capabilities.tools.contains(&tool.name)
        })
        .collect())
}

fn capability_digest(
    view: &agentrs_contracts::authority::CapabilityView,
) -> agentrs_contracts::authority::CapabilityViewDigest {
    let bytes = serde_json::to_vec(view).expect("CapabilityView is serializable");
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    agentrs_contracts::authority::CapabilityViewDigest(agentrs_contracts::ids::Digest::from_hex(format!(
        "{hash:016x}"
    )))
}

/// steering 队列上限。超过后 `submit` 返回 `QueueFull`，避免无界增长。
const STEERING_CAPACITY: usize = 64;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agentrs_contracts::authority::{AuthorityEnvelope, CapabilityView, PermissionMode};
    use agentrs_contracts::ids::{EventSequence, Timestamp};
    use agentrs_contracts::spec::{
        ContextBudget, ConversationSnapshot, ExecutionBudget, ModelPolicy, RunCheckpoint, RunSpec,
        SystemContext,
    };
    use agentrs_contracts::version::{CompatWindow, SpecVersion};
    use agentrs_types::{ContentBlock, LlmEvent, LlmRequest, StopReason, TokenUsage};

    use super::*;
    use crate::engine::{AdmitAll, FixedClock, StepDriver, Termination};

    struct OneShot;

    #[async_trait::async_trait]
    impl StepDriver for OneShot {
        async fn call(&self, _r: LlmRequest) -> Result<Vec<LlmEvent>, String> {
            Ok(vec![
                LlmEvent::TextDelta("ok".into()),
                LlmEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: TokenUsage::default(),
                },
            ])
        }
    }

    fn 规格(run: &str, v: SpecVersion) -> RunSpec {
        RunSpec {
            run_id: run.into(),
            parent_run_id: None,
            conversation: ConversationSnapshot::default(),
            system_context: SystemContext::default(),
            authority: AuthorityEnvelope {
                id: "e".into(),
                workspaces: vec![],
                tools: vec![],
                providers: vec!["p".into()],
                models: vec!["m".into()],
                max_depth: 1,
            },
            initial_capabilities: CapabilityView {
                tools: vec![],
                providers: vec!["p".into()],
                models: vec!["m".into()],
            },
            permission_mode: PermissionMode::Default,
            model_policy: ModelPolicy {
                tiers: [(agentrs_contracts::spec::ModelTier::Default, "m".into())]
                    .into_iter()
                    .collect(),
                fallback: vec![],
                providers: vec!["p".into()],
                max_retries: 0,
                allow_attachments: false,
            },
            context_budget: ContextBudget {
                max_input_tokens: 1000,
                reserved_output_tokens: 100,
                compaction_threshold_pct: 80,
            },
            execution_budget: ExecutionBudget::default(),
            checkpoint: None,
            spec_version: v,
            config: Default::default(),
        }
    }

    fn 依赖(p: Arc<agentrs_testkit::FakePersistence>) -> EngineDeps {
        EngineDeps {
            persistence: p,
            event_sink: None,
            clock: Arc::new(FixedClock(Timestamp(0))),
            driver: Arc::new(OneShot),
            admission: Arc::new(AdmitAll),
            tools: None,
            context: None,
            components: None,
        }
    }

    fn ckpt(v: SpecVersion) -> RunCheckpoint {
        RunCheckpoint {
            spec_version: v,
            up_to_seq: EventSequence(0),
            pending_approval: None,
        }
    }

    #[tokio::test]
    async fn start_从_epoch_1_开始() {
        let host = RuntimeHost::new();
        let p = Arc::new(agentrs_testkit::FakePersistence::new());
        let r = host
            .start(规格("r1", SpecVersion(1)), 依赖(p), TurnGuards::default())
            .await
            .unwrap();
        assert_eq!(r.handle.epoch(), RunEpoch(1));
    }

    #[tokio::test]
    async fn resume_分配更大的_epoch_从而围栏旧_writer() {
        // 这是恢复正确性的核心：旧 writer 若仍活着，其后续写入必被 Fenced。
        let host = RuntimeHost::new();
        let p = Arc::new(agentrs_testkit::FakePersistence::new());

        let first = host
            .start(规格("r1", SpecVersion(1)), 依赖(p.clone()), TurnGuards::default())
            .await
            .unwrap();
        assert_eq!(first.handle.epoch(), RunEpoch(1));

        let second = host
            .resume(
                规格("r1", SpecVersion(1)),
                ckpt(SpecVersion(1)),
                依赖(p.clone()),
                TurnGuards::default(),
            )
            .await
            .unwrap();
        assert_eq!(second.handle.epoch(), RunEpoch(2), "resume 必须推进 epoch");

        // 新 writer 先写一条，旧 writer 随后的写入被围栏。
        second.driver.await;
        let 旧 = first.driver.await;
        assert_eq!(
            旧.termination,
            Termination::Failed {
                code: "fenced".into()
            },
            "旧 writer 必须被围栏并收敛"
        );
    }

    #[tokio::test]
    async fn 不同_run_的_epoch_相互独立() {
        let host = RuntimeHost::new();
        let p = Arc::new(agentrs_testkit::FakePersistence::new());
        let a = host
            .start(规格("r1", SpecVersion(1)), 依赖(p.clone()), TurnGuards::default())
            .await
            .unwrap();
        let b = host
            .start(规格("r2", SpecVersion(1)), 依赖(p), TurnGuards::default())
            .await
            .unwrap();
        assert_eq!(a.handle.epoch(), RunEpoch(1));
        assert_eq!(b.handle.epoch(), RunEpoch(1), "各 Run 独立计数");
    }

    #[tokio::test]
    async fn 过旧的_checkpoint_拒绝恢复而非静默降级() {
        let host = RuntimeHost::with_compat(CompatWindow {
            min_supported: SpecVersion(3),
            current: SpecVersion(5),
        });
        let p = Arc::new(agentrs_testkit::FakePersistence::new());
        let e = host
            .resume(
                规格("r1", SpecVersion(5)),
                ckpt(SpecVersion(1)),
                依赖(p),
                TurnGuards::default(),
            )
            .await;
        assert_eq!(e.err(), Some(StartError::SpecTooOld));
    }

    #[tokio::test]
    async fn 过新的_checkpoint_同样拒绝() {
        // 降级读取会静默丢字段，比拒绝危险得多。
        let host = RuntimeHost::with_compat(CompatWindow {
            min_supported: SpecVersion(1),
            current: SpecVersion(2),
        });
        let p = Arc::new(agentrs_testkit::FakePersistence::new());
        let e = host
            .resume(
                规格("r1", SpecVersion(2)),
                ckpt(SpecVersion(9)),
                依赖(p),
                TurnGuards::default(),
            )
            .await;
        assert_eq!(e.err(), Some(StartError::SpecTooNew));
    }

    #[tokio::test]
    async fn 句柄可在驱动过程中注入输入() {
        let host = RuntimeHost::new();
        let p = Arc::new(agentrs_testkit::FakePersistence::new());
        let r = host
            .start(规格("r1", SpecVersion(1)), 依赖(p), TurnGuards::default())
            .await
            .unwrap();

        // 驱动尚未开始，先注入——它会在第一次 claim 时被认领。
        r.handle
            .submit(UserInput::Message(vec![ContentBlock::text("hi")]))
            .await
            .unwrap();

        let s = r.driver.await;
        assert_eq!(s.termination, Termination::Completed);
        assert!(r.handle.is_terminal());
    }

    #[tokio::test]
    async fn 终态后句柄拒绝新输入() {
        let host = RuntimeHost::new();
        let p = Arc::new(agentrs_testkit::FakePersistence::new());
        let r = host
            .start(规格("r1", SpecVersion(1)), 依赖(p), TurnGuards::default())
            .await
            .unwrap();
        r.driver.await;
        assert!(r.handle.submit(UserInput::Message(vec![])).await.is_err());
    }

    #[tokio::test]
    async fn 句柄取消使驱动收敛() {
        let host = RuntimeHost::new();
        let p = Arc::new(agentrs_testkit::FakePersistence::new());
        let r = host
            .start(规格("r1", SpecVersion(1)), 依赖(p), TurnGuards::default())
            .await
            .unwrap();
        r.handle.cancel();
        let s = r.driver.await;
        assert_eq!(s.termination, Termination::Canceled);
    }

    #[tokio::test]
    async fn host_不_spawn_任务由宿主决定运行时() {
        // driver 是一个待驱动的 future——不 await 它，就什么都不会发生。
        let host = RuntimeHost::new();
        let p = Arc::new(agentrs_testkit::FakePersistence::new());
        let r = host
            .start(规格("r1", SpecVersion(1)), 依赖(p.clone()), TurnGuards::default())
            .await
            .unwrap();

        assert_eq!(p.event_count(), 0, "未驱动时不产生任何事件");
        drop(r.driver);
        assert_eq!(p.event_count(), 0, "丢弃 driver 同样不产生事件");
    }

    #[tokio::test]
    async fn 能力视图不能超出授权信封() {
        let host = RuntimeHost::new();
        let p = Arc::new(agentrs_testkit::FakePersistence::new());
        let mut spec = 规格("r1", SpecVersion(1));
        spec.initial_capabilities.tools.push("Write".into());

        let error = host.start(spec, 依赖(p), TurnGuards::default()).await.err();
        assert_eq!(
            error,
            Some(StartError::OutsideAuthority {
                kind: "tool",
                value: "Write".into(),
            })
        );
        assert_eq!(
            host.known_epoch(&"r1".into()).await,
            None,
            "拒绝启动不能消耗 epoch"
        );
    }

    #[tokio::test]
    async fn 模型策略不能复活已撤回模型() {
        let host = RuntimeHost::new();
        let p = Arc::new(agentrs_testkit::FakePersistence::new());
        let mut spec = 规格("r1", SpecVersion(1));
        spec.initial_capabilities.models.clear();

        let error = host.start(spec, 依赖(p), TurnGuards::default()).await.err();
        assert_eq!(
            error,
            Some(StartError::UnavailableCapability {
                kind: "model",
                value: "m".into(),
            })
        );
    }

    #[tokio::test]
    async fn 工具目录取注册项与当前能力的交集() {
        use std::sync::Mutex as StdMutex;

        struct Capture(Arc<StdMutex<Option<LlmRequest>>>);
        #[async_trait::async_trait]
        impl StepDriver for Capture {
            async fn call(&self, request: LlmRequest) -> Result<Vec<LlmEvent>, String> {
                *self.0.lock().unwrap() = Some(request);
                Ok(vec![LlmEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: TokenUsage::default(),
                }])
            }
        }

        let host = RuntimeHost::new();
        let p = Arc::new(agentrs_testkit::FakePersistence::new());
        let captured = Arc::new(StdMutex::new(None));
        let mut spec = 规格("r1", SpecVersion(1));
        spec.authority.tools = vec!["Read".into(), "Write".into()];
        spec.initial_capabilities.tools = vec!["Read".into()];
        let deps = EngineDeps {
            persistence: p,
            event_sink: None,
            clock: Arc::new(FixedClock(Timestamp(0))),
            driver: Arc::new(Capture(captured.clone())),
            admission: Arc::new(AdmitAll),
            tools: None,
            context: None,
            components: None,
        };
        let run = host
            .start_with_tools(
                spec,
                deps,
                TurnGuards::default(),
                vec![
                    agentrs_types::ToolDef::read_only("Read", "read", serde_json::json!({})),
                    agentrs_types::ToolDef::mutating("Write", "write", serde_json::json!({})),
                    agentrs_types::ToolDef::mutating("Exec", "exec", serde_json::json!({})),
                ],
            )
            .await
            .unwrap();
        run.handle
            .submit(UserInput::Message(vec![ContentBlock::text("go")]))
            .await
            .unwrap();
        run.driver.await;

        let request = captured.lock().unwrap().take().unwrap();
        assert_eq!(
            request
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Read"]
        );
    }

    #[tokio::test]
    async fn 思考进入_surface_而不是被丢掉() {
        // provider 解析出了 reasoning，`legalization` 也准备好按端点能力剥离它，
        // 中间这一段却什么都没做——于是推理模型的思考链在内核里蒸发：屏幕上没有，
        // 下一次请求里也没有。
        use std::sync::Mutex as StdMutex;

        use agentrs_contracts::event::EventPayload;
        use agentrs_types::{Message, Role};

        struct 两轮(Arc<StdMutex<Vec<LlmRequest>>>);
        #[async_trait::async_trait]
        impl StepDriver for 两轮 {
            async fn call(&self, request: LlmRequest) -> Result<Vec<LlmEvent>, String> {
                let first = self.0.lock().unwrap().is_empty();
                self.0.lock().unwrap().push(request);
                if first {
                    Ok(vec![
                        LlmEvent::ThinkingDelta("先看单位".into()),
                        LlmEvent::ThinkingDelta("，再算".into()),
                        LlmEvent::ThinkingSignature("sig-1".into()),
                        LlmEvent::TextDelta("是 25".into()),
                        LlmEvent::Done {
                            stop_reason: StopReason::EndTurn,
                            usage: TokenUsage::default(),
                        },
                    ])
                } else {
                    Ok(vec![LlmEvent::Done {
                        stop_reason: StopReason::EndTurn,
                        usage: TokenUsage::default(),
                    }])
                }
            }
        }

        let host = RuntimeHost::new();
        let persistence = Arc::new(agentrs_testkit::FakePersistence::new());
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let deps = EngineDeps {
            persistence: persistence.clone(),
            event_sink: None,
            clock: Arc::new(FixedClock(Timestamp(0))),
            driver: Arc::new(两轮(seen.clone())),
            admission: Arc::new(AdmitAll),
            tools: None,
            context: None,
            components: None,
        };
        let run = host
            .start(规格("r-think", SpecVersion(1)), deps, TurnGuards::default())
            .await
            .unwrap();
        run.handle
            .submit(UserInput::Message(vec![ContentBlock::text("3²+4²")]))
            .await
            .unwrap();
        run.handle
            .submit(UserInput::Message(vec![ContentBlock::text("再说一次")]))
            .await
            .unwrap();
        run.driver.await;

        // 1. 思考进了 durable Surface，带着它的签名。
        let blocks: Vec<ContentBlock> = persistence
            .events()
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayload::SurfaceMessageRecorded { message } => {
                    serde_json::from_value::<Message>(message.clone()).ok()
                }
                _ => None,
            })
            .filter(|message| message.role == Role::Assistant)
            .flat_map(|message| message.content.into_iter())
            .collect();
        assert!(
            matches!(
                blocks.first(),
                Some(ContentBlock::Thinking { thinking, signature })
                    if thinking == "先看单位，再算" && signature.as_deref() == Some("sig-1")
            ),
            "{blocks:?}"
        );
        // 2. 顺序：思考在正文之前。Anthropic 族顺序错了直接 400。
        assert!(matches!(blocks.get(1), Some(ContentBlock::Text { .. })), "{blocks:?}");

        // 3. 记下来之后，**要不要发回去是端点能力说了算**，不是内核。
        //    默认的 OpenAI 兼容端点不收 thinking 块，`legalization` 在装配时把它
        //    剥掉——这正是那段代码写来干的事，而在此之前它一行都跑不到，因为
        //    从来没有人造出过一个 Thinking 块。Anthropic 族保留并要求签名往返，
        //    见 `provider::legalization` 自己的测试。
        let requests = seen.lock().unwrap();
        let second = requests.last().expect("两次请求");
        assert!(
            !second
                .messages
                .iter()
                .flat_map(|message| message.content.iter())
                .any(|block| matches!(block, ContentBlock::Thinking { .. })),
            "OpenAI 兼容端点不该收到 thinking 块"
        );
    }

    #[tokio::test]
    async fn 分叉出的_run_带着整段对话启动() {
        use std::sync::Mutex as StdMutex;

        use agentrs_contracts::event::{Causality, Durability, EventPayload, Visibility};
        use agentrs_contracts::ids::EventId;
        use agentrs_contracts::spec::ForkSpec;
        use agentrs_contracts::surface::{SurfaceEventKind, SurfaceMarker, SurfaceOp};
        use agentrs_types::{Message, Role};

        struct Capture(Arc<StdMutex<Option<LlmRequest>>>);
        #[async_trait::async_trait]
        impl StepDriver for Capture {
            async fn call(&self, request: LlmRequest) -> Result<Vec<LlmEvent>, String> {
                *self.0.lock().unwrap() = Some(request);
                Ok(vec![LlmEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: TokenUsage::default(),
                }])
            }
        }

        fn 源事件(seq: u64, role: Role, text: &str) -> RunEventEnvelope {
            RunEventEnvelope {
                run_id: "r-src".into(),
                epoch: RunEpoch(1),
                event_id: EventId::new(format!("e{seq}")),
                seq: Some(EventSequence(seq)),
                live_seq: None,
                at: Timestamp(0),
                durability: Durability::DurableFact,
                visibility: Visibility::User,
                causality: Causality::default(),
                surface: Some(SurfaceMarker {
                    kind: SurfaceEventKind::AssistantMessage,
                    op: SurfaceOp::Append,
                }),
                payload: EventPayload::SurfaceMessageRecorded {
                    message: serde_json::to_value(Message::new(
                        role,
                        vec![ContentBlock::text(text)],
                    ))
                    .unwrap(),
                },
            }
        }

        let events = vec![
            源事件(1, Role::User, "第一轮问题"),
            源事件(2, Role::Assistant, "第一轮回答"),
        ];
        let source_spec = 规格("r-src", SpecVersion(1));
        let forked = crate::fork(
            &ForkSpec {
                source_run_id: "r-src".into(),
                new_run_id: "r-next".into(),
                boundary: None,
            },
            &events,
            &source_spec,
            None,
            source_spec.authority.clone(),
        )
        .unwrap();

        let host = RuntimeHost::new();
        let captured = Arc::new(StdMutex::new(None));
        let deps = EngineDeps {
            persistence: Arc::new(agentrs_testkit::FakePersistence::new()),
            event_sink: None,
            clock: Arc::new(FixedClock(Timestamp(0))),
            driver: Arc::new(Capture(captured.clone())),
            admission: Arc::new(AdmitAll),
            tools: None,
            context: None,
            components: None,
        };
        let run = host
            .start_forked(forked.spec, &events, deps, TurnGuards::default(), vec![])
            .await
            .unwrap();
        run.handle
            .submit(UserInput::Message(vec![ContentBlock::text("第二轮问题")]))
            .await
            .unwrap();
        run.driver.await;

        // 这是分叉存在的全部意义：新 Run 的第一次请求里带着上一轮的对话，
        // 而不是从空白开始。
        let request = captured.lock().unwrap().take().unwrap();
        let 正文: Vec<String> = request
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(正文, vec!["第一轮问题", "第一轮回答", "第二轮问题"]);
    }

    #[tokio::test]
    async fn 分叉把继承来的前缀写进新_run_自己的日志() {
        // fork 规则 3：新 Run 自带完整可重建前缀。只把历史塞进内存 Surface，
        // 模型看得见而日志里没有，`--resume` 会重建出一段缺了开头的对话——
        // 而多轮对话每一轮都分叉一次，第三轮就会把第一轮丢掉。
        use agentrs_contracts::event::{Causality, Durability, EventPayload, Visibility};
        use agentrs_contracts::ids::EventId;
        use agentrs_contracts::spec::ForkSpec;
        use agentrs_contracts::surface::{SurfaceEventKind, SurfaceMarker, SurfaceOp};
        use agentrs_types::{Message, Role};

        fn 源事件(seq: u64, role: Role, text: &str) -> RunEventEnvelope {
            RunEventEnvelope {
                run_id: "r-src".into(),
                epoch: RunEpoch(1),
                event_id: EventId::new(format!("e{seq}")),
                seq: Some(EventSequence(seq)),
                live_seq: None,
                at: Timestamp(0),
                durability: Durability::DurableFact,
                visibility: Visibility::User,
                causality: Causality::default(),
                surface: Some(SurfaceMarker {
                    kind: SurfaceEventKind::AssistantMessage,
                    op: SurfaceOp::Append,
                }),
                payload: EventPayload::SurfaceMessageRecorded {
                    message: serde_json::to_value(Message::new(
                        role,
                        vec![ContentBlock::text(text)],
                    ))
                    .unwrap(),
                },
            }
        }

        let events = vec![
            源事件(1, Role::User, "第一轮问题"),
            源事件(2, Role::Assistant, "第一轮回答"),
        ];
        let source_spec = 规格("r-src", SpecVersion(1));
        let forked = crate::fork(
            &ForkSpec {
                source_run_id: "r-src".into(),
                new_run_id: "r-next".into(),
                boundary: None,
            },
            &events,
            &source_spec,
            None,
            source_spec.authority.clone(),
        )
        .unwrap();

        let host = RuntimeHost::new();
        let persistence = Arc::new(agentrs_testkit::FakePersistence::new());
        let run = host
            .start_forked(
                forked.spec,
                &events,
                依赖(persistence.clone()),
                TurnGuards::default(),
                vec![],
            )
            .await
            .unwrap();
        run.handle
            .submit(UserInput::Message(vec![ContentBlock::text("第二轮问题")]))
            .await
            .unwrap();
        run.driver.await;

        let 正文: Vec<String> = persistence
            .events()
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayload::SurfaceMessageRecorded { message } => {
                    serde_json::from_value::<Message>(message.clone()).ok()
                }
                _ => None,
            })
            .flat_map(|message| message.content.into_iter())
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text),
                _ => None,
            })
            .collect();
        // 新 Run 的日志里三条都在，所以它单独重放就能重建整段对话。
        assert_eq!(
            正文,
            vec!["第一轮问题", "第一轮回答", "第二轮问题", "ok"]
        );
    }

    #[tokio::test]
    async fn 继承来的前缀不在_live_通道上重播一次() {
        // 它落进日志是为了让新 Run 自足；再广播一次就是把宿主早已显示在屏幕上的
        // 那段对话贴第二份。恢复也是这么做的：重建 Surface 不重播事件。
        use agentrs_contracts::event::{Causality, Durability, EventPayload, Visibility};
        use agentrs_contracts::ids::EventId;
        use agentrs_contracts::spec::ForkSpec;
        use agentrs_contracts::surface::{SurfaceEventKind, SurfaceMarker, SurfaceOp};
        use agentrs_types::{Message, Role};

        let events: Vec<RunEventEnvelope> = [(1u64, Role::User, "第一轮问题"), (2, Role::Assistant, "第一轮回答")]
            .into_iter()
            .map(|(seq, role, text)| RunEventEnvelope {
                run_id: "r-src".into(),
                epoch: RunEpoch(1),
                event_id: EventId::new(format!("e{seq}")),
                seq: Some(EventSequence(seq)),
                live_seq: None,
                at: Timestamp(0),
                durability: Durability::DurableFact,
                visibility: Visibility::User,
                causality: Causality::default(),
                surface: Some(SurfaceMarker {
                    kind: SurfaceEventKind::AssistantMessage,
                    op: SurfaceOp::Append,
                }),
                payload: EventPayload::SurfaceMessageRecorded {
                    message: serde_json::to_value(Message::new(role, vec![ContentBlock::text(text)]))
                        .unwrap(),
                },
            })
            .collect();
        let source_spec = 规格("r-src", SpecVersion(1));
        let forked = crate::fork(
            &ForkSpec {
                source_run_id: "r-src".into(),
                new_run_id: "r-next".into(),
                boundary: None,
            },
            &events,
            &source_spec,
            None,
            source_spec.authority.clone(),
        )
        .unwrap();

        #[derive(Default)]
        struct 记录 (std::sync::Mutex<Vec<RunEventEnvelope>>);
        #[async_trait::async_trait]
        impl agentrs_contracts::ports::RunEventSink for 记录 {
            async fn publish(
                &self,
                event: RunEventEnvelope,
            ) -> Result<(), agentrs_contracts::ports::EventSinkError> {
                self.0.lock().unwrap().push(event);
                Ok(())
            }
        }

        let sink = Arc::new(记录::default());
        let mut deps = 依赖(Arc::new(agentrs_testkit::FakePersistence::new()));
        deps.event_sink = Some(sink.clone());
        let host = RuntimeHost::new();
        let run = host
            .start_forked(forked.spec, &events, deps, TurnGuards::default(), vec![])
            .await
            .unwrap();
        run.handle
            .submit(UserInput::Message(vec![ContentBlock::text("第二轮问题")]))
            .await
            .unwrap();
        run.driver.await;

        let 广播: Vec<String> = sink
            .0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayload::SurfaceMessageRecorded { message } => {
                    serde_json::from_value::<Message>(message.clone()).ok()
                }
                _ => None,
            })
            .flat_map(|message| message.content.into_iter())
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(广播, vec!["第二轮问题", "ok"], "只广播这一轮说的话");
    }

    #[tokio::test]
    async fn 分叉拒绝没有对话可继续的前缀() {
        let host = RuntimeHost::new();
        let mut spec = 规格("r-next", SpecVersion(1));
        spec.conversation = ConversationSnapshot {
            derived_from: Some("r-src".into()),
            up_to_seq: Some(EventSequence(9)),
        };
        let error = host
            .start_forked(
                spec,
                &[],
                依赖(Arc::new(agentrs_testkit::FakePersistence::new())),
                TurnGuards::default(),
                vec![],
            )
            .await
            .err();
        assert!(
            matches!(error, Some(StartError::InvalidReplay(ref why)) if why.contains("no conversation")),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn 分叉不接受源_run_的_checkpoint() {
        // 规则 5：checkpoint 里可能挂着未决审批，那属于已经结束的那次执行。
        let host = RuntimeHost::new();
        let mut spec = 规格("r-next", SpecVersion(1));
        spec.conversation = ConversationSnapshot {
            derived_from: Some("r-src".into()),
            up_to_seq: Some(EventSequence(1)),
        };
        spec.checkpoint = Some(RunCheckpoint {
            spec_version: SpecVersion(1),
            up_to_seq: EventSequence(1),
            pending_approval: None,
        });
        let error = host
            .start_forked(
                spec,
                &[],
                依赖(Arc::new(agentrs_testkit::FakePersistence::new())),
                TurnGuards::default(),
                vec![],
            )
            .await
            .err();
        assert!(
            matches!(error, Some(StartError::InvalidReplay(ref why)) if why.contains("checkpoint")),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn resume_from_events_恢复_surface_并推进历史_epoch() {
        use std::sync::Mutex as StdMutex;

        use agentrs_contracts::event::{Causality, Durability, EventPayload, Visibility};
        use agentrs_contracts::ports::RunPersistence;
        use agentrs_contracts::surface::{SurfaceEventKind, SurfaceMarker, SurfaceOp};
        use agentrs_types::{Message, Role};

        struct Capture(Arc<StdMutex<Option<LlmRequest>>>);
        #[async_trait::async_trait]
        impl StepDriver for Capture {
            async fn call(&self, request: LlmRequest) -> Result<Vec<LlmEvent>, String> {
                *self.0.lock().unwrap() = Some(request);
                Ok(vec![
                    LlmEvent::TextDelta("resumed".into()),
                    LlmEvent::Done {
                        stop_reason: StopReason::EndTurn,
                        usage: TokenUsage::default(),
                    },
                ])
            }
        }

        fn event(id: &str, payload: EventPayload, surface: Option<SurfaceMarker>) -> RunEventEnvelope {
            RunEventEnvelope {
                run_id: "r-resume".into(),
                epoch: RunEpoch(3),
                event_id: id.into(),
                seq: None,
                live_seq: None,
                at: Timestamp(0),
                durability: Durability::DurableFact,
                visibility: Visibility::User,
                causality: Causality::default(),
                surface,
                payload,
            }
        }

        let persistence = Arc::new(agentrs_testkit::FakePersistence::new());
        let prior = Message::new(Role::User, vec![ContentBlock::text("continue this")]);
        persistence
            .append_event(
                RunEpoch(3),
                event(
                    "old-1",
                    EventPayload::SurfaceMessageRecorded {
                        message: serde_json::to_value(&prior).unwrap(),
                    },
                    Some(SurfaceMarker {
                        kind: SurfaceEventKind::UserMessage,
                        op: SurfaceOp::Append,
                    }),
                ),
            )
            .await
            .unwrap();
        persistence
            .append_event(
                RunEpoch(3),
                event(
                    "old-2",
                    EventPayload::ModelRequestPrepared {
                        request_id: "r-resume-req".into(),
                    },
                    None,
                ),
            )
            .await
            .unwrap();
        let events = persistence.events();
        let captured = Arc::new(StdMutex::new(None));
        let deps = EngineDeps {
            persistence: persistence.clone(),
            event_sink: None,
            clock: Arc::new(FixedClock(Timestamp(0))),
            driver: Arc::new(Capture(captured.clone())),
            admission: Arc::new(AdmitAll),
            tools: None,
            context: None,
            components: None,
        };
        let resumed = RuntimeHost::new()
            .resume_from_events(
                规格("r-resume", SpecVersion(1)),
                RunCheckpoint {
                    spec_version: SpecVersion(1),
                    up_to_seq: EventSequence(2),
                    pending_approval: None,
                },
                &events,
                deps,
                TurnGuards::default(),
                Vec::new(),
            )
            .await
            .unwrap();
        assert_eq!(resumed.handle.epoch(), RunEpoch(4));
        let summary = resumed.driver.await;
        assert_eq!(summary.termination, Termination::Completed);
        let request = captured.lock().unwrap().take().unwrap();
        assert_eq!(request.messages.first(), Some(&prior));
    }

    #[tokio::test]
    async fn resume_from_events_拒绝静默重放部分输出() {
        use agentrs_contracts::event::{Causality, Durability, EventPayload, Visibility};

        let events = vec![
            RunEventEnvelope {
                run_id: "r-partial".into(),
                epoch: RunEpoch(2),
                event_id: "e1".into(),
                seq: Some(EventSequence(1)),
                live_seq: None,
                at: Timestamp(0),
                durability: Durability::DurableFact,
                visibility: Visibility::User,
                causality: Causality::default(),
                surface: None,
                payload: EventPayload::ModelRequestPrepared {
                    request_id: "q".into(),
                },
            },
            RunEventEnvelope {
                run_id: "r-partial".into(),
                epoch: RunEpoch(2),
                event_id: "e2".into(),
                seq: Some(EventSequence(2)),
                live_seq: None,
                at: Timestamp(0),
                durability: Durability::DurableFact,
                visibility: Visibility::User,
                causality: Causality::default(),
                surface: None,
                payload: EventPayload::PartialOutputStarted,
            },
        ];
        let error = RuntimeHost::new()
            .resume_from_events(
                规格("r-partial", SpecVersion(1)),
                RunCheckpoint {
                    spec_version: SpecVersion(1),
                    up_to_seq: EventSequence(2),
                    pending_approval: None,
                },
                &events,
                依赖(Arc::new(agentrs_testkit::FakePersistence::new())),
                TurnGuards::default(),
                Vec::new(),
            )
            .await
            .err();
        assert_eq!(error, Some(StartError::RecoveryNeedsAction("partial_output")));
    }
}
