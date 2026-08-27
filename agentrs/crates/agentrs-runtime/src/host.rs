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

use agentrs_contracts::ids::{RunEpoch, RunId};
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

    /// 启动一次新的 Run。
    pub async fn start(
        &self,
        spec: RunSpec,
        deps: EngineDeps,
        guards: TurnGuards,
    ) -> Result<StartedRun, StartError> {
        self.launch(spec, deps, guards, Vec::new()).await
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
        self.launch(spec, deps, guards, tools).await
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
        self.launch(spec, deps, guards, Vec::new()).await
    }

    async fn launch(
        &self,
        spec: RunSpec,
        deps: EngineDeps,
        guards: TurnGuards,
        tools: Vec<agentrs_types::ToolDef>,
    ) -> Result<StartedRun, StartError> {
        let run_id = spec.run_id.clone();
        let epoch = self.next_epoch(&run_id).await;

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
            .unwrap_or_else(|| "default".into());

        let system_prompt = spec.system_context.sections.join("\n\n");
        let workspace = spec
            .system_context
            .workspace_id
            .clone()
            .unwrap_or_else(|| "default".to_string());

        let engine = Engine::new(
            run_id.clone(),
            epoch,
            deps,
            inbox.clone(),
            cancel.clone(),
            owner,
            guards,
        )
        .with_model(model, system_prompt)
        .with_workspace(workspace, format!("cs-{run_id}"))
        .with_tools(tools)
        // 必须在 with_tools 之后：它要在**完整**目录上做投影。
        .with_permission_mode(spec.permission_mode.clone());

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
                providers: vec![],
                models: vec![],
                max_depth: 1,
            },
            initial_capabilities: CapabilityView {
                tools: vec![],
                providers: vec![],
                models: vec![],
            },
            permission_mode: PermissionMode::Default,
            model_policy: ModelPolicy {
                tiers: Default::default(),
                fallback: vec![],
                providers: vec![],
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
            clock: Arc::new(FixedClock(Timestamp(0))),
            driver: Arc::new(OneShot),
            admission: Arc::new(AdmitAll),
            tools: None,
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
}
