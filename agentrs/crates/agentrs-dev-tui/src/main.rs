//! `agentrs-tui` executable.

use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use agentrs_contracts::authority::{AuthorityEnvelope, CapabilityView, PermissionMode};
use agentrs_contracts::ids::{EventSequence, Timestamp};
use agentrs_contracts::ports::{PolicyEnforcer, RunEventSink, RunPersistence, SandboxExecutor};
use agentrs_contracts::spec::{
    ContextBudget, ConversationSnapshot, ExecutionBudget, ModelPolicy, RunCheckpoint, RunSpec, SystemContext,
};
use agentrs_contracts::version::SpecVersion;
use agentrs_dev_adapter::{
    ApprovalAnswer, ApprovalPrompt, InteractiveDevPolicy, JsonlPersistence, LocalFileSandbox,
};
use agentrs_dev_tui::event_sink::ChannelEventSink;
use agentrs_dev_tui::state::{AppState, RunStatus};
use agentrs_dev_tui::terminal::TerminalGuard;
use agentrs_dev_tui::ui::{render, View};
use agentrs_provider::transport::OpenAiCompatProvider;
use agentrs_provider::ProviderPort;
use agentrs_runtime::engine::{AdmitAll, EngineDeps, FixedClock, RunSummary, StepDriver, TurnGuards};
use agentrs_runtime::host::{RunHandle, RuntimeHost, StartError};
use agentrs_runtime::inbox::UserInput;
use agentrs_runtime::toolround::ToolRoundDeps;
use agentrs_types::{ContentBlock, LlmEvent, LlmRequest, ToolDef};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

struct ProviderDriver(Arc<OpenAiCompatProvider>);

#[async_trait::async_trait]
impl StepDriver for ProviderDriver {
    async fn call(&self, request: LlmRequest) -> Result<Vec<LlmEvent>, String> {
        self.0.stream(request).await.map_err(|error| error.to_string())
    }

    async fn call_stream(
        &self,
        request: LlmRequest,
        emit: Arc<dyn Fn(LlmEvent) + Send + Sync>,
    ) -> Result<(), String> {
        self.0
            .stream_with(request, move |event| emit(event))
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug)]
struct Args {
    workspace: PathBuf,
    log: PathBuf,
    resume: bool,
    prompt: Option<String>,
}

impl Args {
    fn parse() -> Result<Option<Self>, String> {
        let mut workspace = PathBuf::from(".");
        let mut log = None;
        let mut resume = false;
        let mut prompt = Vec::new();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => {
                    print_help();
                    return Ok(None);
                }
                "--workspace" => {
                    workspace = args.next().ok_or("--workspace requires a path")?.into();
                }
                "--log" => log = Some(PathBuf::from(args.next().ok_or("--log requires a path")?)),
                "--resume" => {
                    resume = true;
                    log = Some(PathBuf::from(args.next().ok_or("--resume requires a log path")?));
                }
                value if value.starts_with('-') => return Err(format!("unknown option: {value}")),
                value => prompt.push(value.to_string()),
            }
        }
        let log =
            log.unwrap_or_else(|| PathBuf::from(format!(".agentrs/tui-{}.jsonl", uuid::Uuid::now_v7())));
        Ok(Some(Self {
            workspace,
            log,
            resume,
            prompt: (!prompt.is_empty()).then(|| prompt.join(" ")),
        }))
    }
}

fn print_help() {
    println!(
        "agentrs-tui [OPTIONS] [PROMPT]\n\n\
         Options:\n  --workspace PATH   Workspace exposed to dev tools (default: .)\n\
         \x20 --log PATH         Durable JSONL path for a new run\n\
         \x20 --resume PATH      Replay and resume a durable JSONL run\n\
         \x20 -h, --help         Show this help\n\n\
         Environment:\n  AGENTRS_BASE_URL   OpenAI-compatible /v1 base URL\n\
         \x20 AGENTRS_API_KEY    Provider key (optional for local endpoints)\n\
         \x20 AGENTRS_MODEL      Model identifier"
    );
}

struct ProviderConfig {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
}

impl ProviderConfig {
    fn from_env() -> Self {
        Self {
            base_url: std::env::var("AGENTRS_BASE_URL").ok(),
            api_key: std::env::var("AGENTRS_API_KEY").ok(),
            model: std::env::var("AGENTRS_MODEL").ok(),
        }
    }

    fn model_label(&self) -> &str {
        self.model.as_deref().unwrap_or("<unset>")
    }

    fn validate(&self) -> Result<(&str, &str), String> {
        let base_url = self.base_url.as_deref().ok_or("AGENTRS_BASE_URL is not set")?;
        let model = self.model.as_deref().ok_or("AGENTRS_MODEL is not set")?;
        Ok((base_url, model))
    }
}

struct RecoveryInput {
    events: Vec<agentrs_contracts::event::RunEventEnvelope>,
    checkpoint: RunCheckpoint,
}

struct Session {
    handle: RunHandle,
    task: Option<JoinHandle<RunSummary>>,
    event_rx: mpsc::Receiver<agentrs_contracts::event::RunEventEnvelope>,
    approval_rx: mpsc::Receiver<ApprovalPrompt>,
    dropped_live: Arc<std::sync::atomic::AtomicU64>,
    sandbox: Arc<LocalFileSandbox>,
    change_set: String,
}

async fn start_session(
    args: &Args,
    provider_config: &ProviderConfig,
    persistence: Arc<JsonlPersistence>,
    recovery: Option<RecoveryInput>,
    initial_prompt: Option<&str>,
) -> Result<Session, String> {
    let (base_url, model) = provider_config.validate()?;
    let provider = Arc::new(
        OpenAiCompatProvider::new(base_url, provider_config.api_key.clone())
            .map_err(|error| error.to_string())?,
    );
    let sandbox = Arc::new(LocalFileSandbox::new(&args.workspace).map_err(|error| error.to_string())?);
    let (approval_tx, approval_rx) = mpsc::channel(8);
    let policy = Arc::new(
        InteractiveDevPolicy::new(
            sandbox.clone(),
            ["Read", "Grep"],
            ["Write", "Edit", "Delete"],
            approval_tx,
            Timestamp(0),
            Duration::from_secs(300),
        )
        .map_err(|error| error.to_string())?,
    );
    let tool_round = Arc::new(ToolRoundDeps::minimal(
        policy as Arc<dyn PolicyEnforcer>,
        sandbox.clone() as Arc<dyn SandboxExecutor>,
    ));
    let (event_sink, event_rx, dropped_live) = ChannelEventSink::bounded(512);
    let sink: Arc<dyn RunEventSink> = event_sink;
    let deps = EngineDeps {
        persistence: persistence.clone() as Arc<dyn RunPersistence>,
        event_sink: Some(sink),
        clock: Arc::new(FixedClock(Timestamp(0))),
        driver: Arc::new(ProviderDriver(provider)),
        admission: Arc::new(AdmitAll),
        tools: Some(tool_round),
        context: None,
        components: None,
    };
    let host = RuntimeHost::new();
    let tools = tool_catalog();

    let (run_id, started) = match recovery {
        Some(recovery) => {
            let run_id = recovery
                .events
                .first()
                .map(|event| event.run_id.clone())
                .ok_or("resume log has no events")?;
            let started = host
                .resume_from_events(
                    run_spec(run_id.as_str(), model),
                    recovery.checkpoint,
                    &recovery.events,
                    deps,
                    TurnGuards::default(),
                    tools,
                )
                .await
                .map_err(start_error)?;
            (run_id, started)
        }
        None => {
            let run_id = agentrs_contracts::ids::RunId::new(format!("tui-{}", uuid::Uuid::now_v7()));
            let started = host
                .start_with_tools(
                    run_spec(run_id.as_str(), model),
                    deps,
                    TurnGuards::default(),
                    tools,
                )
                .await
                .map_err(start_error)?;
            (run_id, started)
        }
    };

    if let Some(prompt) = initial_prompt {
        started
            .handle
            .submit(UserInput::Message(vec![ContentBlock::text(prompt)]))
            .await
            .map_err(|error| error.to_string())?;
    }
    let change_set = format!("cs-{run_id}");
    let handle = started.handle;
    let task = tokio::spawn(started.driver);
    Ok(Session {
        handle,
        task: Some(task),
        event_rx,
        approval_rx,
        dropped_live,
        sandbox,
        change_set,
    })
}

fn start_error(error: StartError) -> String {
    format!("resume/start refused: {error}")
}

fn run_spec(run_id: &str, model: &str) -> RunSpec {
    let provider = agentrs_contracts::ids::ProviderId::new("openai-compat");
    let model_id = agentrs_contracts::ids::ModelId::new(model);
    let tools = vec![
        "Read".into(),
        "Grep".into(),
        "Write".into(),
        "Edit".into(),
        "Delete".into(),
    ];
    RunSpec {
        run_id: run_id.into(),
        parent_run_id: None,
        conversation: ConversationSnapshot::default(),
        system_context: SystemContext {
            sections: vec![
                "You are an AgentRS development-test assistant. Use the provided tools when the task requires inspecting or changing workspace files. Mutating tools require explicit human approval."
                    .into(),
            ],
            workspace_id: Some("default".into()),
        },
        authority: AuthorityEnvelope {
            id: "dev-tui".into(),
            workspaces: vec!["default".into()],
            tools: tools.clone(),
            providers: vec![provider.clone()],
            models: vec![model_id.clone()],
            max_depth: 1,
        },
        initial_capabilities: CapabilityView {
            tools,
            providers: vec![provider.clone()],
            models: vec![model_id.clone()],
        },
        permission_mode: PermissionMode::Default,
        model_policy: ModelPolicy {
            tiers: [(agentrs_contracts::spec::ModelTier::Default, model_id)]
                .into_iter()
                .collect(),
            fallback: Vec::new(),
            providers: vec![provider],
            max_retries: 1,
            allow_attachments: false,
        },
        context_budget: ContextBudget {
            max_input_tokens: 100_000,
            reserved_output_tokens: 4_000,
            compaction_threshold_pct: 80,
        },
        execution_budget: ExecutionBudget::default(),
        checkpoint: None,
        spec_version: SpecVersion(1),
        config: Default::default(),
    }
}

fn tool_catalog() -> Vec<ToolDef> {
    vec![
        ToolDef::read_only(
            "Read",
            "Read a UTF-8 text file inside the workspace.",
            serde_json::json!({
                "type":"object",
                "properties":{"path":{"type":"string"}},
                "required":["path"]
            }),
        ),
        ToolDef::read_only(
            "Grep",
            "Search workspace text files for a literal substring.",
            serde_json::json!({
                "type":"object",
                "properties":{"pattern":{"type":"string"}},
                "required":["pattern"]
            }),
        ),
        ToolDef::mutating(
            "Write",
            "Stage complete file content in the current ChangeSet.",
            serde_json::json!({
                "type":"object",
                "properties":{"path":{"type":"string"},"content":{"type":"string"}},
                "required":["path","content"]
            }),
        ),
        ToolDef::mutating(
            "Edit",
            "Replace every occurrence of old with new in a workspace file, staged in the ChangeSet.",
            serde_json::json!({
                "type":"object",
                "properties":{
                    "path":{"type":"string"},
                    "old":{"type":"string"},
                    "new":{"type":"string"}
                },
                "required":["path","old","new"]
            }),
        ),
        ToolDef::mutating(
            "Delete",
            "Stage deletion of one workspace file in the current ChangeSet.",
            serde_json::json!({
                "type":"object",
                "properties":{"path":{"type":"string"}},
                "required":["path"]
            }),
        ),
    ]
}

fn load_recovery(persistence: &JsonlPersistence) -> Result<(RecoveryInput, AppState), Box<dyn Error>> {
    let events = persistence.load_events()?;
    if events.is_empty() {
        return Err("resume log is empty".into());
    }
    let last_seq = events
        .iter()
        .filter(|event| event.is_durable())
        .filter_map(|event| event.seq)
        .max()
        .unwrap_or(EventSequence(0));
    let checkpoint = persistence.load_checkpoint()?.unwrap_or(RunCheckpoint {
        spec_version: SpecVersion(1),
        up_to_seq: last_seq,
        pending_approval: None,
    });
    let mut state = AppState::default();
    state.replay(&events);
    Ok((RecoveryInput { events, checkpoint }, state))
}

fn is_terminal(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Completed | RunStatus::Canceled | RunStatus::NeedsAction | RunStatus::Failed
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(args) = Args::parse().map_err(|error| format!("{error}; use --help"))? else {
        return Ok(());
    };
    let provider = ProviderConfig::from_env();
    let persistence = Arc::new(JsonlPersistence::open(&args.log)?);
    if !args.resume && persistence.event_count() > 0 {
        return Err(format!(
            "new-run log is not empty: {}; use --resume or choose another --log path",
            args.log.display()
        )
        .into());
    }
    let (recovery, mut state) = if args.resume {
        let (recovery, mut state) = load_recovery(&persistence)?;
        state.notice = Some(format!("replayed {} durable events", recovery.events.len()));
        (Some(recovery), state)
    } else {
        (None, AppState::default())
    };
    if provider.base_url.is_none() || provider.model.is_none() {
        let config_notice = "provider config incomplete; set AGENTRS_BASE_URL and AGENTRS_MODEL";
        state.notice = Some(match state.notice.take() {
            Some(existing) => format!("{existing}; {config_notice}"),
            None => config_notice.into(),
        });
    } else if state.notice.is_none() {
        state.notice = Some(format!("durable log: {}", args.log.display()));
    }

    let mut terminal = TerminalGuard::enter()?;
    let mut composer = String::new();
    let mut pending_approval: Option<ApprovalPrompt> = None;
    let mut session: Option<Session> = None;
    let workspace_label = args.workspace.display().to_string();
    let mut cancel_requested = false;

    if let Some(recovery) = recovery {
        if !is_terminal(state.status) {
            match start_session(&args, &provider, persistence.clone(), Some(recovery), None).await {
                Ok(started) => session = Some(started),
                Err(error) => {
                    state.status = RunStatus::NeedsAction;
                    state.notice = Some(error);
                }
            }
        }
    } else if let Some(prompt) = args.prompt.as_deref() {
        match start_session(&args, &provider, persistence.clone(), None, Some(prompt)).await {
            Ok(started) => session = Some(started),
            Err(error) => state.notice = Some(error),
        }
    }

    let mut quit = false;
    while !quit {
        if let Some(active) = session.as_mut() {
            while let Ok(event) = active.event_rx.try_recv() {
                state.apply(&event);
            }
            state.dropped_live = active.dropped_live.load(Ordering::Relaxed);
            if pending_approval.is_none() {
                if let Ok(prompt) = active.approval_rx.try_recv() {
                    state.status = RunStatus::AwaitingApproval;
                    pending_approval = Some(prompt);
                }
            }
            if active.task.as_ref().is_some_and(JoinHandle::is_finished) {
                pending_approval = None;
                let task = active.task.take().expect("checked above");
                match task.await {
                    Ok(summary) => {
                        state.usage = summary.usage;
                        state.notice = Some(format!(
                            "termination={:?}; turns={} steps={}; C commit / D discard / Q quit",
                            summary.termination, summary.turns, summary.steps
                        ));
                    }
                    Err(error) => {
                        state.status = RunStatus::Failed;
                        state.notice = Some(format!("runtime task failed: {error}"));
                    }
                }
            }
        }

        terminal.terminal().draw(|frame| {
            let pending_changes = session
                .as_ref()
                .map(|active| active.sandbox.pending_count(&active.change_set))
                .unwrap_or(0);
            render(
                frame,
                &View {
                    state: &state,
                    composer: &composer,
                    approval: pending_approval.as_ref().map(|prompt| &prompt.request),
                    workspace: &workspace_label,
                    model: provider.model_label(),
                    pending_changes,
                },
            )
        })?;

        while event::poll(Duration::from_millis(0))? {
            match event::read()? {
                Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                    if let Some(prompt) = pending_approval.take() {
                        match approval_answer(&key) {
                            Some(answer) => {
                                if prompt.respond_to.send(answer).is_ok() {
                                    state.status = RunStatus::Running;
                                } else {
                                    state.notice = Some("approval is no longer active".into());
                                }
                            }
                            None => pending_approval = Some(prompt),
                        }
                        continue;
                    }

                    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                        if let Some(active) = &session {
                            if active.task.is_some() {
                                if cancel_requested {
                                    quit = true;
                                } else {
                                    cancel_requested = true;
                                    active.handle.cancel();
                                    state.notice = Some(
                                        "cancellation requested; press Ctrl+C again to force quit".into(),
                                    );
                                }
                            } else {
                                quit = true;
                            }
                        } else {
                            quit = true;
                        }
                        continue;
                    }

                    match key.code {
                        KeyCode::Enter if !composer.trim().is_empty() => {
                            let text = std::mem::take(&mut composer);
                            if let Some(active) = &session {
                                if active.task.is_some() {
                                    if let Err(error) = active
                                        .handle
                                        .submit(UserInput::Message(vec![ContentBlock::text(text)]))
                                        .await
                                    {
                                        state.notice = Some(format!("input rejected: {error}"));
                                    }
                                } else {
                                    state.notice =
                                        Some("run is terminal; press Q to quit and start a new log".into());
                                }
                            } else {
                                match start_session(&args, &provider, persistence.clone(), None, Some(&text))
                                    .await
                                {
                                    Ok(started) => {
                                        cancel_requested = false;
                                        session = Some(started);
                                    }
                                    Err(error) => state.notice = Some(error),
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            composer.pop();
                        }
                        KeyCode::Char('q' | 'Q') if composer.is_empty() && is_terminal(state.status) => {
                            if let Some(active) = &session {
                                if active.sandbox.pending_count(&active.change_set) > 0 {
                                    state.notice =
                                        Some("pending ChangeSet: press C to commit or D to discard".into());
                                } else {
                                    quit = true;
                                }
                            } else {
                                quit = true;
                            }
                        }
                        KeyCode::Char('c' | 'C') if composer.is_empty() && is_terminal(state.status) => {
                            if let Some(active) = &session {
                                match active.sandbox.commit(&active.change_set) {
                                    Ok(count) => {
                                        state.notice = Some(format!("committed {count} ChangeSet entries"))
                                    }
                                    Err(error) => state.notice = Some(format!("commit failed: {error}")),
                                }
                            }
                        }
                        KeyCode::Char('d' | 'D') if composer.is_empty() && is_terminal(state.status) => {
                            if let Some(active) = &session {
                                let count = active.sandbox.discard(&active.change_set);
                                state.notice = Some(format!("discarded {count} ChangeSet entries"));
                            }
                        }
                        KeyCode::Char(ch)
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            composer.push(ch);
                        }
                        _ => {}
                    }
                }
                Event::Paste(text) => composer.push_str(&text),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        tokio::time::sleep(Duration::from_millis(16)).await;
    }
    Ok(())
}

fn approval_answer(key: &KeyEvent) -> Option<ApprovalAnswer> {
    match key.code {
        KeyCode::Char('y' | 'Y') => Some(ApprovalAnswer::AllowOnce {
            user_id: "dev-tui-user".into(),
        }),
        KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(ApprovalAnswer::Reject {
            user_id: "dev-tui-user".into(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_keys_are_explicit() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(matches!(
            approval_answer(&key(KeyCode::Char('y'))),
            Some(ApprovalAnswer::AllowOnce { .. })
        ));
        assert!(matches!(
            approval_answer(&key(KeyCode::Esc)),
            Some(ApprovalAnswer::Reject { .. })
        ));
        assert_eq!(approval_answer(&key(KeyCode::Enter)), None);
    }
}
