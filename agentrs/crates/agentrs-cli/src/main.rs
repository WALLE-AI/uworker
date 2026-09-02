//! `agentrs` —— 轻量命令行驱动器。
//!
//! ```text
//! agentrs doctor                              健康检查：版本、端口、adapter
//! agentrs run <prompt> [dev] [--commit]        跑一次 Run
//! agentrs resume-run [log] [dev]                从 durable log 实际恢复
//! agentrs serve --jsonl                         stdin 命令 -> stdout durable RunEvent
//! agentrs conformance                          跑宿主义务 conformance suite
//!
//! 以下四条**只读**事件日志，不触达 provider、不执行工具，
//! 因此在一个坏掉的 Run 上照样能跑——而那正是最需要它们的时候：
//!
//! ```text
//! agentrs validate      [log]   这份日志本身能不能信
//! agentrs trajectory    [log]   这次 Run 做了什么
//! agentrs cache-report  [log]   钱花在哪、每次 miss 因为什么
//! agentrs replay        [log]   投影是否确定性
//! agentrs resume        [log]   崩溃时正在做什么、要不要先 reconcile
//! agentrs export        [log]   产出一份可对外分享的脱敏 bundle
//! ```
//! ```
//!
//! 默认只装配只读/模拟 adapter；`--adapter dev` 才启用真实的本地执行器，
//! 且那条路径仍走完整的 grant → Sandbox 复核链路。

mod inspect;

use std::collections::BTreeMap;
use std::io::BufRead;
use std::sync::{Arc, Mutex};

use agentrs_contracts::authority::{AuthorityEnvelope, CapabilityView, PermissionMode};
use agentrs_contracts::ids::Timestamp;
use agentrs_contracts::ports::{PolicyEnforcer, RunPersistence, SandboxExecutor};
use agentrs_contracts::spec::{
    ContextBudget, ConversationSnapshot, ExecutionBudget, ModelPolicy, RunSpec, SystemContext,
};
use agentrs_contracts::version::SpecVersion;
use agentrs_dev_adapter::{DevPolicy, JsonlPersistence, LocalDevSandbox, NON_PRODUCTION_BANNER};
use agentrs_provider::routing::{Route, RoutingProvider};
use agentrs_provider::transport::OpenAiCompatProvider;
use agentrs_provider::ProviderPort;
use agentrs_runtime::engine::{AdmitAll, EngineDeps, FixedClock, StepDriver, TurnGuards};
use agentrs_runtime::host::RuntimeHost;
use agentrs_runtime::inbox::UserInput;
use agentrs_runtime::toolround::ToolRoundDeps;
use agentrs_types::{ContentBlock, LlmEvent, LlmRequest};

/// 把 `ProviderPort` 适配成引擎的 `StepDriver`。
///
/// 引擎不直接依赖 provider crate——那会让状态机测试必须构造完整适配器。
struct ProviderDriver(Arc<dyn ProviderPort>);

#[async_trait::async_trait]
impl StepDriver for ProviderDriver {
    async fn call(&self, req: LlmRequest) -> Result<Vec<LlmEvent>, String> {
        if std::env::var("AGENTRS_TRACE").is_ok() {
            eprintln!(
                "[trace] model={} msgs={} tools={}",
                req.model,
                req.messages.len(),
                req.tools.len()
            );
        }
        self.0.stream(req).await.map_err(|e| {
            // 只输出稳定错误码，不透传响应正文（可能含密钥或用户内容）。
            eprintln!("[provider] {e}");
            e.to_string()
        })
    }
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// 对 dev-adapter 跑一遍宿主义务 conformance suite。
///
/// **第三方 adapter 应当照此实现 `SandboxSubject` 并在自家 CI 里跑同一套检查。**
/// 内核不为第三方 adapter 的正确性背书（架构 §12.2）。
async fn conformance(workspace: &str) -> Result<(), Box<dyn std::error::Error>> {
    use agentrs_contracts::ids::ExecutionId;
    use agentrs_contracts::policy::InputHash;
    use agentrs_contracts::sandbox::{ExecutionRequest, IsolationLevel};
    use agentrs_testkit::conformance::{check_sandbox, SandboxSubject};

    struct Subject(Arc<LocalDevSandbox>);

    impl SandboxSubject for Subject {
        fn executor(&self) -> Arc<dyn SandboxExecutor> {
            self.0.clone()
        }
        fn issue_grant(&self, id: &str, bound: InputHash, expires_at: Timestamp) {
            self.0.issue_grant(id, bound, expires_at);
        }
        fn mutating_request(
            &self,
            execution_id: &str,
            input_hash: InputHash,
            required_isolation: IsolationLevel,
        ) -> ExecutionRequest {
            ExecutionRequest {
                execution_id: ExecutionId::new(execution_id),
                tool_name: "Write".into(),
                arguments: serde_json::json!({
                    "path": format!(".agentrs-conformance/{execution_id}.txt"),
                    "content": "x"
                }),
                change_set_id: "cs-conformance".into(),
                input_hash,
                required_isolation,
            }
        }
        fn side_effect_happened(&self, req: &ExecutionRequest) -> Option<bool> {
            // 副作用落在 overlay 里——内核无提交权，磁盘上查不到。
            let p = req.arguments["path"].as_str()?;
            Some(self.0.read_text("cs-conformance", p).is_some())
        }

        // 读己之写与 ChangeSet 隔离（H7）需要一个可配对的读。
        fn paired_read(
            &self,
            execution_id: &str,
            input_hash: InputHash,
            written: &ExecutionRequest,
        ) -> Option<ExecutionRequest> {
            Some(ExecutionRequest {
                execution_id: ExecutionId::new(execution_id),
                tool_name: "Read".into(),
                arguments: serde_json::json!({"path": written.arguments["path"]}),
                change_set_id: written.change_set_id.clone(),
                input_hash,
                required_isolation: written.required_isolation,
            })
        }
        fn read_content(&self, r: &agentrs_contracts::sandbox::ExecutionResult) -> Option<String> {
            r.output.clone()
        }
        fn in_change_set(&self, req: &ExecutionRequest, change_set: &str) -> Option<ExecutionRequest> {
            let mut r = req.clone();
            r.change_set_id = change_set.into();
            Some(r)
        }
    }

    let sb = Arc::new(LocalDevSandbox::new(workspace)?);
    println!("受检对象：agentrs-dev-adapter（L0BasicContainment）");
    println!("工作区根：{}\n", sb.root().display());

    let mut ok = true;
    let report = check_sandbox(&Subject(sb)).await;
    print!("{}", report.render());
    ok &= report.passed();

    // ---- H3：持久化 ----
    {
        use agentrs_contracts::ids::RunEpoch;
        use agentrs_testkit::conformance::persistence::{check_persistence, PersistenceSubject};

        struct P(Arc<JsonlPersistence>);
        impl PersistenceSubject for P {
            fn persistence(&self) -> Arc<dyn RunPersistence> {
                self.0.clone()
            }
            fn advance_epoch(&self, current: RunEpoch) -> Option<RunEpoch> {
                Some(RunEpoch(current.0 + 1))
            }
        }

        // 写进独立文件，不污染真实的事件流。
        let p = Arc::new(JsonlPersistence::open(".agentrs/conformance-events.jsonl")?);
        println!();
        let r = check_persistence(&P(p)).await;
        print!("{}", r.render());
        ok &= r.passed();
    }

    // ---- H5：策略 ----
    {
        use agentrs_contracts::policy::ToolProposal;
        use agentrs_testkit::conformance::policy::{check_policy, PolicySubject};

        struct Pol(Arc<DevPolicy>);
        impl PolicySubject for Pol {
            fn policy(&self) -> Arc<dyn PolicyEnforcer> {
                self.0.clone()
            }
            fn allowed_proposal(&self, tag: &str) -> ToolProposal {
                ToolProposal {
                    step_id: "s-conformance".into(),
                    call_id: tag.into(),
                    tool_name: "Write".into(),
                    arguments: serde_json::json!({"path": "a.txt", "content": "x"}),
                    workspace_id: "default".into(),
                    change_set_id: "cs-conformance".into(),
                    input_hash: agentrs_contracts::policy::InputHash(
                        agentrs_contracts::ids::Digest::from_hex(tag),
                    ),
                }
            }
        }

        let sb2 = Arc::new(LocalDevSandbox::new(workspace)?);
        let pol = Arc::new(DevPolicy::new(sb2, ["Write"]));
        println!();
        let r = check_policy(&Pol(pol)).await;
        print!("{}", r.render());
        ok &= r.passed();
    }

    if !ok {
        return Err("conformance 不合格".into());
    }
    Ok(())
}

fn doctor() {
    println!("agentrs {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("契约 SpecVersion  : {:?}", SpecVersion(1));
    println!("隔离级别（dev）   : L0BasicContainment（真实隔离归 SandboxRS）");
    println!();
    println!("环境：");
    for k in [
        "AGENTRS_BASE_URL",
        "AGENTRS_MODEL",
        "AGENTRS_WORKSPACE",
        "AGENTRS_LOG_PATH",
    ] {
        match env(k) {
            Some(v) => println!("  {k:18} = {v}"),
            None => println!("  {k:18} = (未设置)"),
        }
    }
    println!();
    println!("adapter：");
    println!("  default          只读/模拟，不触达任何真实资源");
    println!("  dev              本地 JSONL + 受限文件执行器（L0）");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Human,
    JsonlEvents,
}

type RecoveryInput = (
    Vec<agentrs_contracts::event::RunEventEnvelope>,
    agentrs_contracts::spec::RunCheckpoint,
    agentrs_contracts::ids::RunId,
);

async fn run(prompt: &str, use_dev: bool, commit: bool) -> Result<(), Box<dyn std::error::Error>> {
    execute_run(Some(prompt), use_dev, commit, None, "cli-run", OutputMode::Human).await
}

async fn resume_run(log: &str, use_dev: bool) -> Result<(), Box<dyn std::error::Error>> {
    execute_run(None, use_dev, false, Some(log), "cli-run", OutputMode::Human).await
}

async fn execute_run(
    prompt: Option<&str>,
    use_dev: bool,
    commit: bool,
    recovery_log: Option<&str>,
    requested_run_id: &str,
    output_mode: OutputMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let base_url = env("AGENTRS_BASE_URL").ok_or("需要 AGENTRS_BASE_URL")?;
    let model = env("AGENTRS_MODEL").ok_or("需要 AGENTRS_MODEL")?;
    let workspace = env("AGENTRS_WORKSPACE").unwrap_or_else(|| ".".to_string());
    let max_retries = env("AGENTRS_MAX_RETRIES")
        .map(|value| value.parse::<u8>())
        .transpose()?
        .unwrap_or(1);

    let primary_provider: Arc<dyn ProviderPort> =
        Arc::new(OpenAiCompatProvider::new(&base_url, env("AGENTRS_API_KEY"))?);
    let mut adapters = BTreeMap::from([(
        agentrs_contracts::ids::ProviderId::new("openai-compat"),
        primary_provider,
    )]);
    let mut fallback_routes = Vec::new();
    let fallback_model = env("AGENTRS_FALLBACK_MODEL");
    let fallback_base_url = env("AGENTRS_FALLBACK_BASE_URL");
    match (&fallback_model, &fallback_base_url) {
        (Some(fallback_model), Some(fallback_base_url)) => {
            adapters.insert(
                "openai-compat-fallback".into(),
                Arc::new(OpenAiCompatProvider::new(
                    fallback_base_url,
                    env("AGENTRS_FALLBACK_API_KEY"),
                )?),
            );
            fallback_routes.push(Route::new(fallback_model.as_str(), "openai-compat-fallback"));
        }
        (None, None) => {}
        _ => return Err("fallback 需要同时设置 AGENTRS_FALLBACK_MODEL 与 AGENTRS_FALLBACK_BASE_URL".into()),
    }
    let allowed_models = std::iter::once(model.clone())
        .chain(fallback_model.clone())
        .map(Into::into)
        .collect::<Vec<_>>();
    let allowed_providers = std::iter::once(agentrs_contracts::ids::ProviderId::new("openai-compat"))
        .chain(fallback_model.as_ref().map(|_| "openai-compat-fallback".into()))
        .collect::<Vec<_>>();
    let provider: Arc<dyn ProviderPort> = Arc::new(RoutingProvider::new(
        Route::new(model.as_str(), "openai-compat"),
        fallback_routes,
        &allowed_models,
        &allowed_providers,
        adapters,
        max_retries,
    )?);

    // 内核无提交权：ChangeSet 的提交是**宿主的显式动作**。
    let mut committer: Option<Arc<LocalDevSandbox>> = None;

    // ---- 装配工具执行 adapter ----
    let tools: Option<Arc<ToolRoundDeps>> = if use_dev {
        eprintln!("{NON_PRODUCTION_BANNER}");

        let sandbox = Arc::new(LocalDevSandbox::new(&workspace)?);
        eprintln!("工作区根：{}", sandbox.root().display());
        committer = Some(sandbox.clone());

        // allowlist 是白名单——不在名单内一律拒绝。名字取自目录本身而不是手抄：
        // 抄漏一个（这里从前就漏了 Glob），那个工具永远调不动，而模型只会看到
        // 一次次 OutOfAuthority，然后换着法子绕。
        let policy = Arc::new(DevPolicy::new(
            sandbox.clone(),
            agentrs_dev_adapter::env_tool_names(),
        ));

        let t = Arc::new(
            ToolRoundDeps::minimal(
                policy as Arc<dyn PolicyEnforcer>,
                sandbox as Arc<dyn SandboxExecutor>,
            )
            .with_guard(Arc::new(agentrs_dev_adapter::CommandShapeGuard)),
        );
        Some(t)
    } else {
        None
    };

    // Recovery always uses the selected durable JSONL file. New dev runs use the
    // default log; non-dev new runs retain the existing no-I/O behavior.
    let (persistence, recovery): (Arc<dyn RunPersistence>, Option<RecoveryInput>) =
        if let Some(path) = recovery_log {
            let adapter = Arc::new(JsonlPersistence::open(path)?);
            let events = adapter.load_events()?;
            let run_id = events
                .first()
                .map(|event| event.run_id.clone())
                .ok_or("事件日志为空，无法恢复")?;
            let last_seq = events
                .iter()
                .filter(|event| event.is_durable())
                .filter_map(|event| event.seq)
                .max()
                .ok_or("事件日志没有 durable 事件")?;
            let checkpoint = adapter
                .load_checkpoint()?
                .unwrap_or(agentrs_contracts::spec::RunCheckpoint {
                    spec_version: SpecVersion(1),
                    up_to_seq: last_seq,
                    pending_approval: None,
                });
            (adapter, Some((events, checkpoint, run_id)))
        } else if use_dev {
            (
                Arc::new(JsonlPersistence::open(
                    env("AGENTRS_LOG_PATH").unwrap_or_else(|| ".agentrs/events.jsonl".into()),
                )?),
                None,
            )
        } else if output_mode == OutputMode::JsonlEvents {
            (Arc::new(StdoutEventPersistence::default()), None)
        } else {
            (Arc::new(NullPersistence), None)
        };

    let host = RuntimeHost::new();
    let deps = EngineDeps {
        persistence,
        event_sink: None,
        clock: Arc::new(FixedClock(Timestamp(0))),
        driver: Arc::new(ProviderDriver(provider)),
        admission: Arc::new(AdmitAll),
        tools,
        context: None,
        components: None,
    };

    // 工具目录由宿主提供——内核不持有实现，也不自行发现工具。
    //
    // 与执行侧同一处来源（`agentrs_dev_adapter::catalog`）：schema 说 offset 是整数、
    // 执行侧却按字符串读，模型永远得不到它想要的结果，而这种漂移只会在真跑一次之后
    // 才暴露。两个宿主各抄一份的时候，改一处忘一处是迟早的事。
    let tools = if use_dev {
        agentrs_dev_adapter::env_tool_catalog()
    } else {
        Vec::new()
    };

    let run_id = recovery
        .as_ref()
        .map(|(_, _, run_id)| run_id.as_str())
        .unwrap_or(requested_run_id);
    let run_spec = spec(run_id, &model, fallback_model.as_deref(), max_retries);
    let started = match recovery {
        Some((events, checkpoint, _)) => {
            host.resume_from_events(run_spec, checkpoint, &events, deps, TurnGuards::default(), tools)
                .await
        }
        None => {
            host.start_with_tools(run_spec, deps, TurnGuards::default(), tools)
                .await
        }
    }
    .map_err(|e| e.to_string())?;

    if let Some(prompt) = prompt {
        started
            .handle
            .submit(UserInput::Message(vec![ContentBlock::text(prompt)]))
            .await
            .map_err(|e| e.to_string())?;
    }

    let summary = started.driver.await;

    if output_mode == OutputMode::Human {
        println!();
        // **先给答案再给统计。** 只打统计的 CLI 等于跑完不说结果。
        match &summary.final_text {
            Some(t) => println!("{t}\n"),
            None => println!("（本次没有助手文本输出）\n"),
        }
        println!("终止  : {:?}", summary.termination);
        println!(
            "Turn  : {}（其中 0-Step {}）",
            summary.turns, summary.zero_step_turns
        );
        println!("Step  : {}", summary.steps);
    }

    if let Some(sb) = committer {
        let cs = "cs-cli-run";
        let pending = sb.pending_count(cs);
        if output_mode == OutputMode::Human {
            println!("待提交: {pending} 个文件（未提交时不落盘）");
        }
        if commit && pending > 0 {
            // **这是宿主的动作，不是内核的**——内核只能提议与消费结果。
            let n = sb.commit(cs)?;
            if output_mode == OutputMode::Human {
                println!("已提交: {n} 个文件");
            }
        } else if pending > 0 && output_mode == OutputMode::Human {
            println!("提示  : 加 --commit 以落盘");
        }
    }
    Ok(())
}

fn spec(run_id: &str, model: &str, fallback_model: Option<&str>, max_retries: u8) -> RunSpec {
    let models: Vec<agentrs_contracts::ids::ModelId> = std::iter::once(model)
        .chain(fallback_model)
        .map(Into::into)
        .collect();
    let providers: Vec<agentrs_contracts::ids::ProviderId> =
        std::iter::once(agentrs_contracts::ids::ProviderId::new("openai-compat"))
            .chain(fallback_model.map(|_| "openai-compat-fallback".into()))
            .collect();
    RunSpec {
        run_id: run_id.into(),
        parent_run_id: None,
        conversation: ConversationSnapshot::default(),
        system_context: SystemContext {
            sections: vec![
                "你是一个简洁的助手。需要了解工作区时先用 Glob 或 Grep 找文件，再用 Read 读；\
                 需要改动时用 Write 或 Edit。"
                    .to_string(),
            ],
            workspace_id: Some("default".to_string()),
        },
        authority: AuthorityEnvelope {
            id: "cli".into(),
            workspaces: vec!["default".into()],
            tools: agentrs_dev_adapter::env_tool_names(),
            providers: providers.clone(),
            models: models.clone(),
            max_depth: 1,
        },
        initial_capabilities: CapabilityView {
            tools: agentrs_dev_adapter::env_tool_names(),
            providers: providers.clone(),
            models: models.clone(),
        },
        permission_mode: PermissionMode::Default,
        model_policy: ModelPolicy {
            tiers: [(agentrs_contracts::spec::ModelTier::Default, model.into())]
                .into_iter()
                .collect(),
            fallback: fallback_model.into_iter().map(Into::into).collect(),
            providers,
            max_retries,
            allow_attachments: false,
        },
        context_budget: ContextBudget {
            max_input_tokens: 100_000,
            reserved_output_tokens: 4_000,
            compaction_threshold_pct: 80,
        },
        execution_budget: ExecutionBudget::default(),
        change_set_id: None,
        checkpoint: None,
        spec_version: SpecVersion(1),
        config: Default::default(),
    }
}

/// 默认 adapter：不落盘、不触达任何真实资源。
struct NullPersistence;

/// `serve --jsonl` 的事件出口。它只投影 durable RunEvent，不输出第二种事实格式。
struct StdoutEventPersistence {
    state: Mutex<StdoutState>,
}

struct StdoutState {
    next_seq: u64,
    epoch: agentrs_contracts::ids::RunEpoch,
    by_id: BTreeMap<agentrs_contracts::ids::EventId, agentrs_contracts::ids::EventSequence>,
}

impl Default for StdoutEventPersistence {
    fn default() -> Self {
        Self {
            state: Mutex::new(StdoutState {
                next_seq: 0,
                epoch: agentrs_contracts::ids::RunEpoch(0),
                by_id: BTreeMap::new(),
            }),
        }
    }
}

impl StdoutEventPersistence {
    fn accept_epoch(
        state: &mut StdoutState,
        epoch: agentrs_contracts::ids::RunEpoch,
    ) -> Result<(), agentrs_contracts::ports::PersistError> {
        if epoch < state.epoch {
            return Err(agentrs_contracts::ports::PersistError::Fenced);
        }
        state.epoch = epoch;
        Ok(())
    }
}

#[async_trait::async_trait]
impl RunPersistence for StdoutEventPersistence {
    async fn begin_step(
        &self,
        epoch: agentrs_contracts::ids::RunEpoch,
        _intent: agentrs_contracts::StepIntent,
    ) -> Result<(), agentrs_contracts::ports::PersistError> {
        let mut state = self.state.lock().expect("stdout persistence lock poisoned");
        Self::accept_epoch(&mut state, epoch)
    }

    async fn append_event(
        &self,
        epoch: agentrs_contracts::ids::RunEpoch,
        mut event: agentrs_contracts::event::RunEventEnvelope,
    ) -> Result<agentrs_contracts::ids::EventSequence, agentrs_contracts::ports::PersistError> {
        let mut state = self.state.lock().expect("stdout persistence lock poisoned");
        Self::accept_epoch(&mut state, epoch)?;
        if let Some(seq) = state.by_id.get(&event.event_id) {
            return Ok(*seq);
        }
        state.next_seq += 1;
        let seq = agentrs_contracts::ids::EventSequence(state.next_seq);
        event.epoch = epoch;
        event.seq = Some(seq);
        let line =
            serde_json::to_string(&event).map_err(|_| agentrs_contracts::ports::PersistError::Backend {
                message: "event_serialization_failed".into(),
            })?;
        println!("{line}");
        state.by_id.insert(event.event_id, seq);
        Ok(seq)
    }

    async fn finish_step(
        &self,
        epoch: agentrs_contracts::ids::RunEpoch,
        _result: agentrs_contracts::StepResult,
    ) -> Result<(), agentrs_contracts::ports::PersistError> {
        let mut state = self.state.lock().expect("stdout persistence lock poisoned");
        Self::accept_epoch(&mut state, epoch)
    }

    async fn save_checkpoint(
        &self,
        epoch: agentrs_contracts::ids::RunEpoch,
        checkpoint: agentrs_contracts::spec::RunCheckpoint,
    ) -> Result<(), agentrs_contracts::ports::PersistError> {
        let mut state = self.state.lock().expect("stdout persistence lock poisoned");
        Self::accept_epoch(&mut state, epoch)?;
        if checkpoint.up_to_seq.0 > state.next_seq {
            return Err(agentrs_contracts::ports::PersistError::CheckpointAhead);
        }
        Ok(())
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ServeCommand {
    run_id: String,
    prompt: String,
    #[serde(default)]
    adapter: Option<String>,
    #[serde(default)]
    commit: bool,
}

async fn serve_jsonl() -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let command: ServeCommand = serde_json::from_str(&line)?;
        if command.run_id.trim().is_empty() || command.prompt.trim().is_empty() {
            return Err("serve command requires non-empty run_id and prompt".into());
        }
        let use_dev = match command.adapter.as_deref() {
            None | Some("default") => false,
            Some("dev") => true,
            Some(_) => return Err("serve adapter must be default or dev".into()),
        };
        execute_run(
            Some(&command.prompt),
            use_dev,
            command.commit,
            None,
            &command.run_id,
            OutputMode::JsonlEvents,
        )
        .await?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl RunPersistence for NullPersistence {
    async fn begin_step(
        &self,
        _e: agentrs_contracts::ids::RunEpoch,
        _i: agentrs_contracts::StepIntent,
    ) -> Result<(), agentrs_contracts::ports::PersistError> {
        Ok(())
    }
    async fn append_event(
        &self,
        _e: agentrs_contracts::ids::RunEpoch,
        _ev: agentrs_contracts::event::RunEventEnvelope,
    ) -> Result<agentrs_contracts::ids::EventSequence, agentrs_contracts::ports::PersistError> {
        Ok(agentrs_contracts::ids::EventSequence(0))
    }
    async fn finish_step(
        &self,
        _e: agentrs_contracts::ids::RunEpoch,
        _r: agentrs_contracts::StepResult,
    ) -> Result<(), agentrs_contracts::ports::PersistError> {
        Ok(())
    }
    async fn save_checkpoint(
        &self,
        _e: agentrs_contracts::ids::RunEpoch,
        _c: agentrs_contracts::spec::RunCheckpoint,
    ) -> Result<(), agentrs_contracts::ports::PersistError> {
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("doctor");

    match cmd {
        "doctor" => doctor(),
        "validate" | "trajectory" | "cache-report" | "replay" | "resume" | "export" => {
            let log = args
                .get(1)
                .cloned()
                .unwrap_or_else(|| ".agentrs/events.jsonl".to_string());
            let r = match cmd {
                "validate" => inspect::validate(&log),
                "trajectory" => inspect::trajectory(&log, 20),
                "cache-report" => inspect::cache_report(&log),
                "replay" => inspect::replay(&log),
                "export" => inspect::export(
                    &log,
                    env("AGENTRS_WORKSPACE"),
                    // 盐由调用方给——内核不读随机源（§1.1 边界判据）。
                    &env("AGENTRS_BUNDLE_SALT").unwrap_or_else(|| "default-salt".into()),
                ),
                _ => inspect::resume_plan(&log),
            };
            if let Err(e) = r {
                eprintln!("失败：{e}");
                std::process::exit(1);
            }
        }
        "conformance" => {
            let ws = env("AGENTRS_WORKSPACE").unwrap_or_else(|| ".".to_string());
            if let Err(e) = conformance(&ws).await {
                eprintln!("失败：{e}");
                std::process::exit(1);
            }
        }
        "resume-run" => {
            let use_dev = args.iter().any(|arg| arg == "dev");
            let log = args
                .iter()
                .skip(1)
                .find(|arg| *arg != "dev" && !arg.starts_with("--"))
                .cloned()
                .unwrap_or_else(|| ".agentrs/events.jsonl".to_string());
            if let Err(error) = resume_run(&log, use_dev).await {
                eprintln!("失败：{error}");
                std::process::exit(1);
            }
        }
        "serve" if args.iter().any(|argument| argument == "--jsonl") => {
            if let Err(error) = serve_jsonl().await {
                eprintln!("失败：{error}");
                std::process::exit(1);
            }
        }
        "run" => {
            let use_dev = args.iter().any(|a| a == "dev");
            let prompt = args
                .iter()
                .skip(1)
                .find(|a| !a.starts_with("--") && *a != "dev")
                .cloned()
                .unwrap_or_else(|| "你好".to_string());
            let commit = args.iter().any(|a| a == "--commit");
            if let Err(e) = run(&prompt, use_dev, commit).await {
                eprintln!("失败：{e}");
                std::process::exit(1);
            }
        }
        other => {
            eprintln!("未知命令：{other}");
            eprintln!("用法：");
            eprintln!("  agentrs doctor");
            eprintln!("  agentrs run <prompt> [dev] [--commit]");
            eprintln!("  agentrs resume-run [事件日志] [dev]");
            eprintln!("  agentrs serve --jsonl");
            eprintln!("  agentrs conformance");
            eprintln!("  agentrs validate|trajectory|cache-report|replay|resume|export [事件日志]");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use agentrs_contracts::event::{Causality, Durability, EventPayload, RunEventEnvelope, Visibility};
    use agentrs_contracts::ids::{RunEpoch, Timestamp};

    use super::*;

    fn event(id: &str) -> RunEventEnvelope {
        RunEventEnvelope {
            run_id: "serve-test".into(),
            epoch: RunEpoch(1),
            event_id: id.into(),
            seq: None,
            live_seq: None,
            at: Timestamp(0),
            durability: Durability::DurableFact,
            visibility: Visibility::User,
            causality: Causality::default(),
            surface: None,
            payload: EventPayload::RunStarted,
        }
    }

    #[tokio::test]
    async fn stdout_事件出口满足幂等_单调与_epoch_围栏() {
        let persistence = StdoutEventPersistence::default();
        let first = persistence.append_event(RunEpoch(1), event("e1")).await.unwrap();
        let duplicate = persistence.append_event(RunEpoch(1), event("e1")).await.unwrap();
        let second = persistence.append_event(RunEpoch(1), event("e2")).await.unwrap();
        assert_eq!(first, duplicate);
        assert!(second > first);
        persistence.append_event(RunEpoch(2), event("e3")).await.unwrap();
        assert!(matches!(
            persistence.append_event(RunEpoch(1), event("stale")).await,
            Err(agentrs_contracts::ports::PersistError::Fenced)
        ));
        assert_eq!(persistence.state.lock().unwrap().by_id.len(), 3);
    }

    #[test]
    fn serve_命令拒绝未知字段() {
        let input = r#"{"run_id":"r","prompt":"p","authority":"widen"}"#;
        assert!(serde_json::from_str::<ServeCommand>(input).is_err());
    }
}
