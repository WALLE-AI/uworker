//! `agentrs-tui` executable.
//!
//! The loop is deliberately thin: it moves events into the projection, asks the
//! pure modules what the frame looks like, and turns key presses into
//! [`Action`]s. Nothing here decides what anything looks like, and nothing here
//! matches on a raw key — that is [`agentrs_dev_tui::keymap`]'s job, so the
//! shortcut sheet and the resolver can never disagree.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use agentrs_contracts::authority::{AuthorityEnvelope, CapabilityView, PermissionMode};
use agentrs_contracts::ids::{EventSequence, Timestamp};
use agentrs_contracts::ports::{PolicyEnforcer, RunEventSink, RunPersistence, SandboxExecutor};
use agentrs_contracts::spec::{
    ContextBudget, ConversationSnapshot, ExecutionBudget, ModelPolicy, RunCheckpoint, RunSpec,
    SystemContext,
};
use agentrs_contracts::version::SpecVersion;
use agentrs_dev_adapter::{
    ApprovalAnswer, ApprovalPrompt, InteractiveDevPolicy, JsonlPersistence, LocalFileSandbox,
};
use agentrs_dev_tui::approval::{self, ApprovalOption};
use agentrs_dev_tui::capabilities;
use agentrs_dev_tui::collapse::collapse_runs;
use agentrs_dev_tui::completion::{self, Completion, CompletionKind};
use agentrs_dev_tui::composer::{Composer, Deletion, Motion};
use agentrs_dev_tui::diff::{build_file_diff, DiffOptions, FileDiff};
use agentrs_dev_tui::event_sink::ChannelEventSink;
use agentrs_dev_tui::glyphs::{glyph_set, GlyphSet};
use agentrs_dev_tui::host_io;
use agentrs_dev_tui::keybindings;
use agentrs_dev_tui::keymap::{Action, Chord, Context, Key, Keymap};
use agentrs_dev_tui::notices::{Notice, NoticeQueue, Priority};
use agentrs_dev_tui::overlay::{Overlay, Surface};
use agentrs_dev_tui::spinner::spinner_frame;
use agentrs_dev_tui::state::{AppState, RunStatus, ToolStatus};
use agentrs_dev_tui::surfaces;
use agentrs_dev_tui::terminal::TerminalGuard;
use agentrs_dev_tui::theme::Theme;
use agentrs_dev_tui::tool_card::{kind_of, CardKind};
use agentrs_dev_tui::transcript::{
    build_entries, entry_folded, transcript_rows, EntryKind, TranscriptEntry, TranscriptOptions,
    TranscriptRow,
};
use agentrs_dev_tui::ui::{render, ApprovalView, View};
use agentrs_dev_tui::working_line::{build_working_line, WorkingLine, WorkingLineInput};
use agentrs_provider::transport::OpenAiCompatProvider;
use agentrs_provider::ProviderPort;
use agentrs_runtime::engine::{AdmitAll, EngineDeps, FixedClock, RunSummary, StepDriver, TurnGuards};
use agentrs_runtime::host::{RunHandle, RuntimeHost, StartError};
use agentrs_runtime::inbox::UserInput;
use agentrs_runtime::toolround::ToolRoundDeps;
use agentrs_types::{ContentBlock, LlmEvent, LlmRequest, ToolDef};
use crossterm::event::{self, Event, KeyEventKind};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// The ChangeSet id used to read a file as it is on disk.
///
/// Nothing is ever staged under it, so an overlay lookup falls straight through
/// to the filesystem. That is what gives a diff its "before".
const DISK: &str = "__disk_baseline__";

/// Context window the dev run is given, and what the pressure bar measures.
const CONTEXT_WINDOW: u64 = 100_000;

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
    color: bool,
    permission: PermissionMode,
}

fn parse_permission(value: &str) -> Result<PermissionMode, String> {
    match value {
        "plan" | "read-only" => Ok(PermissionMode::Plan),
        "default" | "workspace-write" => Ok(PermissionMode::Default),
        "accepted" | "danger-full-access" => Ok(PermissionMode::Accepted {
            scopes: vec!["workspace-write".into()],
        }),
        other => Err(format!(
            "unknown permission preset: {other}; use plan, default, or accepted"
        )),
    }
}

impl Args {
    fn parse() -> Result<Option<Self>, String> {
        let mut workspace = PathBuf::from(".");
        let mut log = None;
        let mut resume = false;
        let mut color = true;
        let mut permission = PermissionMode::Default;
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
                "--no-color" => color = false,
                "--permission" => {
                    permission =
                        parse_permission(&args.next().ok_or("--permission requires a preset")?)?;
                }
                value if value.starts_with('-') => return Err(format!("unknown option: {value}")),
                value => prompt.push(value.to_string()),
            }
        }
        let log = log
            .unwrap_or_else(|| PathBuf::from(format!(".agentrs/tui-{}.jsonl", uuid::Uuid::now_v7())));
        Ok(Some(Self {
            workspace,
            log,
            resume,
            prompt: (!prompt.is_empty()).then(|| prompt.join(" ")),
            color,
            permission,
        }))
    }
}

fn print_help() {
    println!(
        "agentrs-tui [OPTIONS] [PROMPT]\n\n\
         Options:\n  --workspace PATH      Workspace exposed to dev tools (default: .)\n\
         \x20 --log PATH            Durable JSONL path for a new run\n\
         \x20 --resume PATH         Replay and resume a durable JSONL run\n\
         \x20 --permission PRESET   plan | default | accepted (default: default)\n\
         \x20 --no-color            Disable colour (NO_COLOR and TERM=dumb are honored too)\n\
         \x20 -h, --help            Show this help\n\n\
         Environment:\n  AGENTRS_BASE_URL      OpenAI-compatible /v1 base URL\n\
         \x20 AGENTRS_API_KEY       Provider key (optional for local endpoints)\n\
         \x20 AGENTRS_MODEL         Model identifier\n\n\
         Press ? inside the TUI for the shortcut sheet."
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

/// Where a session comes from.
enum Origin {
    /// A new conversation.
    Fresh,
    /// A run that was interrupted and is being picked up where it stopped.
    Resume(RecoveryInput),
    /// A turn that continues a run which has already finished.
    ///
    /// The kernel's unit is a Run, and a Run ends when its inbox drains — so a
    /// follow-up message is a *new* Run carrying the old one's conversation.
    /// That is what [`agentrs_runtime::fork`] is for.
    Continue {
        source: agentrs_contracts::ids::RunId,
        events: Vec<agentrs_contracts::event::RunEventEnvelope>,
    },
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

/// One approval, and where the user is in its answer list.
struct PendingApproval {
    prompt: ApprovalPrompt,
    options: Vec<ApprovalOption>,
    selected: usize,
}

async fn start_session(
    args: &Args,
    permission: &PermissionMode,
    provider_config: &ProviderConfig,
    persistence: Arc<JsonlPersistence>,
    origin: Origin,
    reuse: Option<Arc<LocalFileSandbox>>,
    initial_prompt: Option<&str>,
) -> Result<Session, String> {
    let (base_url, model) = provider_config.validate()?;
    let provider = Arc::new(
        OpenAiCompatProvider::new(base_url, provider_config.api_key.clone())
            .map_err(|error| error.to_string())?,
    );
    // A follow-up turn keeps the same sandbox, so every ChangeSet the session
    // has opened stays reachable from one place and one `c` can commit them all.
    // The ChangeSet *id* is still the kernel's to name — a forked Run inherits
    // no live state (fork rule 5), and staged writes are live state.
    let sandbox = match &reuse {
        Some(sandbox) => sandbox.clone(),
        None => Arc::new(LocalFileSandbox::new(&args.workspace).map_err(|error| error.to_string())?),
    };
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

    let (run_id, started) = match origin {
        Origin::Continue { source, events } => {
            let run_id = agentrs_contracts::ids::RunId::new(format!("tui-{}", uuid::Uuid::now_v7()));
            let source_spec = run_spec(source.as_str(), model, permission);
            let forked = agentrs_runtime::fork(
                &agentrs_contracts::spec::ForkSpec {
                    source_run_id: source,
                    new_run_id: run_id.clone(),
                    boundary: None,
                },
                &events,
                &source_spec,
                None,
                source_spec.authority.clone(),
            )
            .map_err(|error| format!("cannot continue this conversation: {error}"))?;
            let started = host
                .start_forked(forked.spec, &events, deps, TurnGuards::default(), tools)
                .await
                .map_err(start_error)?;
            (run_id, started)
        }
        Origin::Resume(recovery) => {
            let run_id = recovery
                .events
                .first()
                .map(|event| event.run_id.clone())
                .ok_or("resume log has no events")?;
            let started = host
                .resume_from_events(
                    run_spec(run_id.as_str(), model, permission),
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
        Origin::Fresh => {
            let run_id = agentrs_contracts::ids::RunId::new(format!("tui-{}", uuid::Uuid::now_v7()));
            let started = host
                .start_with_tools(
                    run_spec(run_id.as_str(), model, permission),
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
    // Must match what the host gave the engine, or a commit would look in an
    // empty ChangeSet and report nothing staged.
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

fn run_spec(run_id: &str, model: &str, permission: &PermissionMode) -> RunSpec {
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
        permission_mode: permission.clone(),
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
            max_input_tokens: CONTEXT_WINDOW,
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

/// Everything the loop owns that is not the runtime itself.
struct App {
    state: AppState,
    composer: Composer,
    notices: NoticeQueue,
    keymap: Keymap,
    /// Fold overrides, by entry id. A membership means "not the default".
    toggled: HashSet<String>,
    /// First visible transcript row, or `None` while following the tail.
    scroll: Option<usize>,
    /// Live diffs by call id, and the content each path was last diffed against.
    diffs: HashMap<String, FileDiff>,
    baselines: HashMap<String, Option<String>>,
    permission: PermissionMode,
    /// Every ChangeSet this session has opened, oldest first.
    ///
    /// A turn is a Run and a Run gets its own ChangeSet, but a *session* is what
    /// the person is having, and their one `c` has to cover all of it.
    change_sets: Vec<String>,
    glyphs: GlyphSet,
    theme: Theme,
    approval: Option<PendingApproval>,
    overlay: Option<Overlay>,
    /// The transcript screen's text box is typing into the search term.
    overlay_searching: bool,
    completion: Option<Completion>,
    /// A file the user asked to open in `$EDITOR`, handled where the terminal is.
    pending_editor: Option<PathBuf>,
    /// The argument of the slash command currently being run, if it had one.
    command_argument: Option<String>,
    /// The wheel belongs to the app; `/mouse` hands it back to the terminal.
    mouse_captured: bool,
    /// Set when the terminal's mouse capture needs to follow `mouse_captured`.
    mouse_dirty: bool,
    /// The durable log this session is writing to.
    current_log: PathBuf,
    turn_started_ms: Option<u64>,
    cancel_armed: bool,
    quit: bool,
    /// Rows of the last frame, so a key can act on what the reader is seeing.
    entries: Vec<TranscriptEntry>,
    rows: Vec<TranscriptRow>,
    viewport: usize,
    columns: usize,
}

impl App {
    fn notify(&mut self, notice: Notice) {
        self.notices.push(notice, host_io::now_ms());
    }

    fn context(&self) -> Context {
        match (&self.overlay, &self.approval) {
            (Some(overlay), _) => surfaces::context_of(overlay.surface),
            (None, Some(_)) => Context::Approval,
            _ => Context::Composer,
        }
    }

    /// Rebuilds the entries and rows the next frame will show.
    fn reproject(&mut self, now_ms: u64) {
        let options = TranscriptOptions {
            glyphs: &self.glyphs,
            now_ms,
            diffs: &self.diffs,
        };
        let entries = collapse_runs(build_entries(&self.state, &options), &self.glyphs);
        self.rows = transcript_rows(&entries, self.columns, &self.toggled, &self.glyphs);
        self.entries = entries;
    }

    /// The first row to draw, following the tail unless the reader scrolled.
    fn scroll_top(&self) -> usize {
        let tail = self.rows.len().saturating_sub(self.viewport);
        self.scroll.map_or(tail, |at| at.min(tail))
    }

    fn scroll_by(&mut self, delta: isize) {
        let tail = self.rows.len().saturating_sub(self.viewport);
        let current = self.scroll_top() as isize;
        let next = (current + delta).clamp(0, tail as isize) as usize;
        // Landing on the tail resumes following, so new output keeps arriving.
        self.scroll = (next < tail).then_some(next);
    }

    /// The last entry on screen matching `wanted`.
    fn focused_entry(&self, wanted: impl Fn(&TranscriptEntry) -> bool) -> Option<&TranscriptEntry> {
        let visible: HashSet<&str> = self
            .rows
            .iter()
            .skip(self.scroll_top())
            .take(self.viewport)
            .map(|row| row.entry_id.as_str())
            .collect();
        self.entries
            .iter()
            .rev()
            .find(|entry| visible.contains(entry.id.as_str()) && wanted(entry))
    }

    /// What `ctrl+o` acts on: the last thing in view that can fold at all.
    ///
    /// Not just tool cards — a `∴ Thinking` block prints `… ctrl+o` under itself
    /// and has to answer to it, or the row is a lie.
    fn focused_foldable(&self) -> Option<&TranscriptEntry> {
        self.focused_entry(|entry| entry.foldable)
    }

    /// What `ctrl+x` acts on: the last card in view that named a file.
    fn focused_location(&self) -> Option<&TranscriptEntry> {
        self.focused_entry(|entry| entry.kind == EntryKind::Tool && !entry.locations.is_empty())
    }

    /// Recomputes the staged diff for every settled mutating call.
    ///
    /// Each path is diffed against what it looked like when its previous card
    /// was built, so two edits to one file read as two changes rather than one
    /// cumulative one.
    fn refresh_diffs(&mut self, session: &Session) {
        let pending: Vec<(String, String)> = self
            .state
            .nodes
            .iter()
            .filter_map(|node| match node {
                agentrs_dev_tui::state::Node::Tool(tool)
                    if tool.status == ToolStatus::Succeeded
                        && kind_of(&tool.name) == CardKind::Diff
                        && !self.diffs.contains_key(&tool.call_id) =>
                {
                    let path = tool.input.get("path")?.as_str()?.to_string();
                    Some((tool.call_id.clone(), path))
                }
                _ => None,
            })
            .collect();
        for (call_id, path) in pending {
            let before = self
                .baselines
                .entry(path.clone())
                .or_insert_with(|| session.sandbox.read_text(DISK, &path))
                .clone();
            let after = session.sandbox.read_text(&session.change_set, &path);
            let diff = build_file_diff(&path, before.as_deref(), after.as_deref(), DiffOptions::default());
            self.baselines.insert(path, after);
            self.diffs.insert(call_id, diff);
        }
    }

    /// Notes a ChangeSet this session has opened.
    fn track_change_set(&mut self, change_set: &str) {
        if !self.change_sets.iter().any(|held| held == change_set) {
            self.change_sets.push(change_set.to_string());
        }
    }

    /// True while a run is actually executing.
    ///
    /// Distinct from "the status is not terminal": before the first message
    /// there is no run at all, and nothing to wait for.
    fn run_in_flight(&self, session: Option<&Session>) -> bool {
        session.is_some_and(|active| active.task.is_some()) && !self.state.status.terminal()
    }

    /// Entries staged across every ChangeSet this session has opened.
    fn pending_changes(&self, session: Option<&Session>) -> usize {
        let Some(session) = session else { return 0 };
        self.change_sets
            .iter()
            .map(|change_set| session.sandbox.pending_count(change_set))
            .sum()
    }

    /// The staged diff of every ChangeSet this session has opened.
    ///
    /// Oldest first, which is the order a commit applies them in: what is read
    /// here is what will land.
    fn staged_changes(&self, session: Option<&Session>) -> Vec<FileDiff> {
        let Some(session) = session else {
            return Vec::new();
        };
        self.change_sets
            .iter()
            .flat_map(|change_set| session.sandbox.pending_entries(change_set))
            .map(|change| {
                build_file_diff(
                    &change.relative,
                    change.on_disk.as_deref(),
                    change.staged.as_deref(),
                    DiffOptions::default(),
                )
            })
            .collect()
    }

    /// What this session is, for `/status`.
    fn status_fields(
        &self,
        session: Option<&Session>,
        provider: &ProviderConfig,
        args: &Args,
    ) -> Vec<surfaces::StatusField> {
        let field = |name: &'static str, value: String| surfaces::StatusField { name, value };
        let mut fields = vec![
            field("run", self.state.run_id.as_ref().map_or("—".into(), ToString::to_string)),
            field("status", self.state.status.label().into()),
            field("turns", self.state.turn.to_string()),
            field("log", self.current_log.display().to_string()),
            field("workspace", args.workspace.display().to_string()),
            field("model", provider.model_label().into()),
            field(
                "endpoint",
                provider.base_url.clone().unwrap_or_else(|| "—".into()),
            ),
            field("permission", approval::mode_label(&self.permission).into()),
            field(
                "tokens",
                format!(
                    "in {} · out {} · cache read {}",
                    self.state.usage.input_tokens,
                    self.state.usage.output_tokens,
                    self.state.usage.cache_read_tokens
                ),
            ),
            field(
                "tools",
                format!(
                    "{} calls · {} approvals · {} cache breaks",
                    self.state.counters.tools,
                    self.state.counters.approvals,
                    self.state.counters.cache_breaks
                ),
            ),
            field(
                "staged",
                format!(
                    "{} entries across {} ChangeSet(s)",
                    self.pending_changes(session),
                    self.change_sets.len()
                ),
            ),
            field("glyphs", if self.glyphs.pending == ">" { "ascii".into() } else { "unicode".into() }),
            field("color", if self.theme.enabled() { "on".into() } else { "off".into() }),
            field("mouse", if self.mouse_captured { "app".into() } else { "terminal".into() }),
        ];
        if self.state.dropped_live > 0 {
            fields.push(field("dropped", format!("{} live events", self.state.dropped_live)));
        }
        fields
    }

    /// The transcript as markdown, for `/export`.
    ///
    /// It is built from the same rows that are on screen — folding included —
    /// so the file says what the reader saw, not a second rendering of it that
    /// can disagree.
    fn export_markdown(&self) -> String {
        let mut out = String::from("# agentrs-tui transcript\n\n");
        out.push_str(&format!("- log: `{}`\n", self.current_log.display()));
        if let Some(run) = &self.state.run_id {
            out.push_str(&format!("- run: `{run}`\n"));
        }
        out.push_str(&format!("- status: {}\n\n", self.state.status.label()));
        out.push_str("```\n");
        for row in &self.rows {
            out.push_str(&row.text());
            out.push('\n');
        }
        out.push_str("```\n");
        out
    }

    /// Opens a full-screen surface.
    fn open_surface(&mut self, surface: Surface) {
        let logs = host_io::list_logs(&self.current_log);
        let current = self
            .current_log
            .file_name()
            .map(|name| name.to_string_lossy().to_string());
        self.overlay = Some(surfaces::open(
            surface,
            &self.keymap,
            &logs,
            current.as_deref(),
            &self.rows,
            self.columns,
        ));
        self.overlay_searching = false;
    }

    /// Recomputes the open completion from the draft, or closes it.
    ///
    /// Called after anything that can move the caret, so the list is never one
    /// keystroke behind what has been typed.
    fn refresh_completion(&mut self, workspace: &Path) {
        let previous = self
            .completion
            .as_ref()
            .and_then(|open| open.current().map(str::to_string));
        let Some((kind, start, query)) =
            completion::detect(self.composer.draft(), self.composer.cursor())
        else {
            self.completion = None;
            return;
        };
        let items = match kind {
            CompletionKind::Command => surfaces::commands_starting_with(&query)
                .iter()
                .map(|entry| entry.name.to_string())
                .collect(),
            CompletionKind::Path => {
                let (prefix, _) = completion::split_path_query(&query);
                let (files, dirs) = host_io::list_dir(&workspace.join(prefix));
                completion::path_items(&query, &files, &dirs)
            }
        };
        if items.is_empty() {
            self.completion = None;
            return;
        }
        // Keep the cursor on whatever it was on when the list only narrowed.
        let selected = previous
            .and_then(|held| items.iter().position(|item| *item == held))
            .unwrap_or(0);
        self.completion = Some(Completion {
            kind,
            start,
            query,
            items,
            selected,
        });
    }

    /// The working line, while there is work to describe.
    fn working_line(&self, now_ms: u64, busy: bool) -> Option<WorkingLine> {
        if !busy {
            return None;
        }
        // Nothing is working while a decision is pending: the run is stopped on
        // the human. A turning spinner and a rising elapsed field beside a
        // question would read as a timer on the answer, which it is not.
        if self.approval.is_some() || self.state.status == RunStatus::AwaitingApproval {
            return None;
        }
        let started = self.turn_started_ms?;
        let active = self.state.active_tool();
        let activity = active.map(|tool| {
            agentrs_dev_tui::tool_card::build_tool_card(
                tool,
                &agentrs_dev_tui::tool_card::CardExtras::default(),
            )
            .activity
        });
        let unicode = self.glyphs.pending != ">";
        Some(build_working_line(
            &WorkingLineInput {
                frame: spinner_frame(now_ms, unicode),
                turn: self.state.turn,
                elapsed_ms: now_ms.saturating_sub(started),
                tokens: Some(self.state.usage.output_tokens),
                separator: if unicode { "·" } else { "-" },
                ellipsis: self.glyphs.fold,
                activity: activity.as_deref(),
                silent_ms: self
                    .state
                    .last_output_ms
                    .map(|last| now_ms.saturating_sub(last)),
                tool_running: active.is_some_and(|tool| tool.status == ToolStatus::Running),
            },
            self.columns,
        ))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(args) = Args::parse().map_err(|error| format!("{error}; use --help"))? else {
        return Ok(());
    };
    let provider = ProviderConfig::from_env();
    let mut persistence = Arc::new(JsonlPersistence::open(&args.log)?);
    if !args.resume && persistence.event_count() > 0 {
        return Err(format!(
            "new-run log is not empty: {}; use --resume or choose another --log path",
            args.log.display()
        )
        .into());
    }

    let interactive = host_io::is_interactive();
    if !interactive {
        // Better than a blank frame and a hung process: the TUI needs a terminal
        // it can put into raw mode, and a pipe is not one.
        return Err("stdin and stdout must both be terminals; agentrs-tui is interactive".into());
    }
    let capabilities = capabilities::detect(&host_io::probe_environment(interactive));
    let glyphs = glyph_set(capabilities.unicode);
    let theme = Theme::resolve(capabilities.color_level, args.color);

    let (recovery, state) = if args.resume {
        let (recovery, state) = load_recovery(&persistence)?;
        (Some(recovery), state)
    } else {
        (None, AppState::default())
    };

    let mut app = App {
        state,
        composer: Composer::default(),
        notices: NoticeQueue::default(),
        keymap: Keymap::default(),
        toggled: HashSet::new(),
        scroll: None,
        diffs: HashMap::new(),
        baselines: HashMap::new(),
        permission: args.permission.clone(),
        change_sets: Vec::new(),
        glyphs,
        theme,
        approval: None,
        overlay: None,
        overlay_searching: false,
        completion: None,
        pending_editor: None,
        command_argument: None,
        mouse_captured: true,
        mouse_dirty: false,
        current_log: args.log.clone(),
        turn_started_ms: None,
        cancel_armed: false,
        quit: false,
        entries: Vec::new(),
        rows: Vec::new(),
        viewport: 1,
        columns: 80,
    };

    // Rebinding is read once, at startup. Anything wrong with the file is a
    // notice and nothing more: the TUI is where the user would go to fix it.
    if let Some(path) = host_io::config_dir().map(|dir| dir.join("keybindings.json")) {
        if let Some(source) = host_io::read_text(&path) {
            let applied = keybindings::apply(&mut app.keymap, &source);
            if !applied.rebound.is_empty() {
                app.notify(
                    Notice::keyed(
                        "keybindings",
                        format!("rebound {} action(s) from keybindings.json", applied.rebound.len()),
                    )
                    .with_priority(Priority::Low),
                );
            }
            for problem in applied.problems {
                app.notify(
                    Notice::keyed(format!("keybindings:{problem}"), problem)
                        .with_priority(Priority::High),
                );
            }
        }
    }

    for note in &capabilities.notes {
        app.notify(Notice::keyed(format!("cap:{note}"), note).with_priority(Priority::Low));
    }
    if let Some(recovery) = &recovery {
        app.notify(
            Notice::keyed(
                "resumed",
                format!("replayed {} durable events", recovery.events.len()),
            )
            .with_priority(Priority::High),
        );
    }
    if provider.base_url.is_none() || provider.model.is_none() {
        app.notify(
            Notice::keyed(
                "provider",
                "provider config incomplete; set AGENTRS_BASE_URL and AGENTRS_MODEL",
            )
            .error(),
        );
    } else {
        app.notify(
            Notice::keyed("log", format!("durable log: {}", args.log.display()))
                .with_priority(Priority::Low),
        );
    }

    let mut session: Option<Session> = None;
    let workspace_label = args.workspace.display().to_string();

    if let Some(recovery) = recovery {
        if !app.state.status.terminal() {
            match start_session(
                &args,
                &app.permission,
                &provider,
                persistence.clone(),
                Origin::Resume(recovery),
                None,
                None,
            )
            .await
            {
                Ok(started) => {
                    app.track_change_set(&started.change_set);
                    session = Some(started);
                }
                Err(error) => {
                    app.state.status = RunStatus::NeedsAction;
                    app.notify(Notice::keyed("start", error).error());
                }
            }
        }
    } else if let Some(prompt) = args.prompt.as_deref() {
        match start_session(
            &args,
            &app.permission,
            &provider,
            persistence.clone(),
            Origin::Fresh,
            None,
            Some(prompt),
        )
        .await
        {
            Ok(started) => {
                app.turn_started_ms = Some(host_io::now_ms());
                app.track_change_set(&started.change_set);
                session = Some(started);
            }
            Err(error) => app.notify(Notice::keyed("start", error).error()),
        }
    }

    let mut terminal = TerminalGuard::enter()?;

    while !app.quit {
        let now = host_io::now_ms();
        app.notices.tick(now);

        if let Some(active) = session.as_mut() {
            while let Ok(event) = active.event_rx.try_recv() {
                app.state.apply(&event, now);
            }
            app.state.dropped_live = active.dropped_live.load(Ordering::Relaxed);
            if app.approval.is_none() {
                if let Ok(prompt) = active.approval_rx.try_recv() {
                    app.state.status = RunStatus::AwaitingApproval;
                    let options = approval::options(&app.permission);
                    let selected = approval::fail_closed_index(&options);
                    app.approval = Some(PendingApproval {
                        prompt,
                        options,
                        selected,
                    });
                }
            }
            if active.task.as_ref().is_some_and(JoinHandle::is_finished) {
                app.approval = None;
                let task = active.task.take().expect("checked above");
                match task.await {
                    Ok(summary) => {
                        app.state.usage = summary.usage;
                        app.notify(
                            Notice::keyed(
                                "finished",
                                format!(
                                    "{:?} · {} turns · {} steps",
                                    summary.termination, summary.turns, summary.steps
                                ),
                            )
                            .with_priority(Priority::High)
                            .invalidating(["start"]),
                        );
                    }
                    Err(error) => {
                        app.state.status = RunStatus::Failed;
                        app.notify(Notice::keyed("start", format!("runtime task failed: {error}")).error());
                    }
                }
            }
            let active = session.as_ref().expect("session is present");
            app.refresh_diffs(active);
        }

        let busy = session
            .as_ref()
            .is_some_and(|active| active.task.is_some())
            && !app.state.status.terminal();
        let working = app.working_line(now, busy);
        let pending_changes = app.pending_changes(session.as_ref());

        terminal.terminal().draw(|frame| {
            let area = frame.area();
            app.columns = area.width as usize;
            // The furniture is measured by the renderer; the viewport is what is
            // left, and a one-row error is not worth a second layout pass.
            app.viewport = area.height.saturating_sub(4) as usize;
            app.reproject(now);
            let scroll = app.scroll_top();
            let overlay_view = app.overlay.as_ref();
            let approval_view = app.approval.as_ref().map(|pending| ApprovalView {
                request: &pending.prompt.request,
                options: &pending.options,
                selected: pending.selected,
            });
            render(
                frame,
                &View {
                    state: &app.state,
                    rows: &app.rows,
                    scroll,
                    composer: &app.composer,
                    approval: approval_view,
                    overlay: overlay_view,
                    completion: app.completion.as_ref(),
                    working: working.as_ref(),
                    notices: &app.notices,
                    workspace: &workspace_label,
                    model: provider.model_label(),
                    permission: &app.permission,
                    pending_changes,
                    context_window: Some(CONTEXT_WINDOW),
                    glyphs: &app.glyphs,
                    theme: app.theme,
                },
            );
        })?;

        while event::poll(Duration::from_millis(0))? {
            match event::read()? {
                Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                    let Some(chord) = Chord::from_event(&key) else {
                        continue;
                    };
                    let context = app.context();
                    let action = app.keymap.resolve(context, chord);
                    handle_key(
                        &mut app,
                        &args,
                        &provider,
                        &mut persistence,
                        &mut session,
                        context,
                        chord,
                        action,
                    )
                    .await?;
                }
                Event::Paste(text) => app.composer.insert(&text),
                Event::Mouse(mouse) if app.mouse_captured => match mouse.kind {
                    crossterm::event::MouseEventKind::ScrollUp => app.scroll_by(-3),
                    crossterm::event::MouseEventKind::ScrollDown => app.scroll_by(3),
                    _ => {}
                },
                _ => {}
            }
        }

        if app.mouse_dirty {
            app.mouse_dirty = false;
            let _ = terminal.set_mouse_capture(app.mouse_captured);
        }

        if let Some(path) = app.pending_editor.take() {
            // The editor gets the terminal to itself; the frame is redrawn from
            // scratch when it hands it back.
            let outcome = terminal.suspended(|| host_io::open_editor(&path, None))?;
            match outcome {
                Ok(()) => app.notify(
                    Notice::keyed("editor", format!("closed {}", path.display())).immediate(),
                ),
                Err(error) => app.notify(Notice::keyed("editor", error).error()),
            }
        }

        tokio::time::sleep(Duration::from_millis(16)).await;
    }
    Ok(())
}

/// Applies one key.
#[allow(clippy::too_many_arguments, reason = "the loop owns the state this needs")]
async fn handle_key(
    app: &mut App,
    args: &Args,
    provider: &ProviderConfig,
    persistence: &mut Arc<JsonlPersistence>,
    session: &mut Option<Session>,
    context: Context,
    chord: Chord,
    action: Option<Action>,
) -> Result<(), Box<dyn Error>> {
    if app.overlay.is_some() {
        handle_overlay_key(app, args, provider, persistence, session, chord, action).await;
        return Ok(());
    }

    // An approval is answered before anything else: while one is open, the run
    // is stopped waiting for exactly this.
    if context == Context::Approval {
        if let Key::Char(digit @ '1'..='9') = chord.key {
            if let Some(index) = approval::by_position(
                &app.approval.as_ref().expect("approval context").options,
                digit.to_digit(10).unwrap_or(0),
            ) {
                answer_approval(app, index);
                return Ok(());
            }
        }
        match action {
            Some(Action::ApprovalAllow) => {
                let index = app
                    .approval
                    .as_ref()
                    .and_then(|pending| {
                        pending.options.iter().position(|option| {
                            option.allowed && option.then_mode.is_none() && !option.accepts_feedback
                        })
                    })
                    .unwrap_or(0);
                answer_approval(app, index);
            }
            Some(Action::ApprovalReject) => {
                let index = approval::fail_closed_index(
                    &app.approval.as_ref().expect("approval context").options,
                );
                answer_approval(app, index);
            }
            Some(Action::ListPrevious) => {
                if let Some(pending) = app.approval.as_mut() {
                    pending.selected = pending.selected.saturating_sub(1);
                }
            }
            Some(Action::ListNext) => {
                if let Some(pending) = app.approval.as_mut() {
                    pending.selected = (pending.selected + 1).min(pending.options.len() - 1);
                }
            }
            Some(Action::ListAccept) => {
                let index = app.approval.as_ref().expect("approval context").selected;
                answer_approval(app, index);
            }
            Some(Action::Escape) => {
                let index = approval::fail_closed_index(
                    &app.approval.as_ref().expect("approval context").options,
                );
                answer_approval(app, index);
            }
            Some(Action::Cancel) => cancel(app, session),
            _ => {}
        }
        return Ok(());
    }

    match action {
        Some(Action::Cancel) => cancel(app, session),
        Some(Action::Escape) => {
            if !app.composer.is_empty() {
                app.composer.clear();
            } else {
                cancel(app, session);
            }
        }
        Some(Action::Submit) => {
            // An open completion takes Enter first — but only while it still has
            // something to add. Once the draft already *is* the candidate,
            // `/status` + enter must run the command rather than costing a
            // second keystroke that visibly does nothing.
            let completes = app.completion.as_ref().is_some_and(|open| {
                open.current().is_some_and(|item| item != open.query)
            });
            if completes {
                return Box::pin(handle_key(
                    app,
                    args,
                    provider,
                    persistence,
                    session,
                    context,
                    chord,
                    Some(Action::CompletionAccept),
                ))
                .await;
            }
            let Some(text) = app.composer.submit() else {
                return Ok(());
            };
            // A draft that is a command runs it rather than being sent to the
            // model: `/commit` is an instruction to this host, not to the run.
            if let Some(rest) = text.strip_prefix('/') {
                let (name, argument) = match rest.trim().split_once(char::is_whitespace) {
                    Some((name, argument)) => (name, argument.trim()),
                    None => (rest.trim(), ""),
                };
                let Some(command) = surfaces::command(name) else {
                    app.notify(
                        Notice::keyed("command", format!("unknown command: /{name}"))
                            .error()
                            .immediate(),
                    );
                    return Ok(());
                };
                if !argument.is_empty() && command.argument.is_none() {
                    app.notify(
                        Notice::keyed(
                            "command",
                            format!("/{name} takes no argument"),
                        )
                        .error()
                        .immediate(),
                    );
                    return Ok(());
                }
                app.command_argument = (!argument.is_empty()).then(|| argument.to_string());
                let Some(command_action) = command.action else {
                    return Ok(());
                };
                let outcome = Box::pin(handle_key(
                    app,
                    args,
                    provider,
                    persistence,
                    session,
                    context,
                    chord,
                    Some(command_action),
                ))
                .await;
                // The argument belongs to the one command that was just run.
                app.command_argument = None;
                return outcome;
            }
            // Only a real message is remembered: `↑` and `/retry` are for things
            // you might want to say again, not for commands.
            app.composer.remember(&text);
            submit_text(app, args, provider, persistence, session, text).await;
        }
        Some(Action::Newline) => app.composer.newline(),
        Some(Action::CaretLeft) => app.composer.move_caret(Motion::Left),
        Some(Action::CaretRight) => app.composer.move_caret(Motion::Right),
        Some(Action::CaretWordLeft) => app.composer.move_caret(Motion::WordLeft),
        Some(Action::CaretWordRight) => app.composer.move_caret(Motion::WordRight),
        Some(Action::CaretLineStart) => app.composer.move_caret(Motion::LineStart),
        Some(Action::CaretLineEnd) => app.composer.move_caret(Motion::LineEnd),
        Some(Action::DeleteBack) => app.composer.delete(Deletion::BackChar),
        Some(Action::DeleteForward) => app.composer.delete(Deletion::ForwardChar),
        Some(Action::DeleteWord) => app.composer.delete(Deletion::BackWord),
        Some(Action::DeleteToLineStart) => app.composer.delete(Deletion::ToLineStart),
        Some(Action::DeleteToLineEnd) => app.composer.delete(Deletion::ToLineEnd),
        // Vertical keys move within a multi-line draft first and only walk the
        // history once there is nowhere left to go.
        Some(Action::HistoryPrevious) if app.completion.is_some() => {
            if let Some(open) = app.completion.as_mut() {
                open.move_by(-1);
            }
        }
        Some(Action::HistoryNext) if app.completion.is_some() => {
            if let Some(open) = app.completion.as_mut() {
                open.move_by(1);
            }
        }
        Some(Action::HistoryPrevious) => {
            if app.composer.caret_line().index > 0 {
                app.composer.move_caret(Motion::Up);
            } else {
                app.composer.walk_history(true);
            }
        }
        Some(Action::HistoryNext) => {
            let caret = app.composer.caret_line();
            if caret.index + 1 < caret.count {
                app.composer.move_caret(Motion::Down);
            } else {
                app.composer.walk_history(false);
            }
        }
        Some(Action::ScrollUp) => app.scroll_by(-1),
        Some(Action::ScrollDown) => app.scroll_by(1),
        Some(Action::ScrollPageUp) => app.scroll_by(-(app.viewport as isize)),
        Some(Action::ScrollPageDown) => app.scroll_by(app.viewport as isize),
        Some(Action::FoldToggle) => match app.focused_foldable() {
            Some(entry) => {
                let id = entry.id.clone();
                let was = entry_folded(entry, app.columns, &app.toggled);
                if !app.toggled.remove(&id) {
                    app.toggled.insert(id);
                }
                app.notify(
                    Notice::keyed("fold", if was { "card opened" } else { "card folded" })
                        .immediate()
                        .for_ms(2_000),
                );
            }
            None => app.notify(
                Notice::keyed("fold", "nothing in view folds")
                    .immediate()
                    .for_ms(2_000),
            ),
        },
        Some(Action::PermissionCycle) => {
            // `/permission plan` names a mode; the key and the bare command
            // cycle to the next one.
            match app.command_argument.clone() {
                Some(wanted) => match parse_permission(&wanted) {
                    Ok(mode) => app.permission = mode,
                    Err(error) => {
                        app.notify(Notice::keyed("permission", error).error().immediate());
                        return Ok(());
                    }
                },
                None => app.permission = approval::next_mode(&app.permission),
            }
            let label = approval::mode_label(&app.permission);
            let text = if session.as_ref().is_some_and(|active| active.task.is_some()) {
                format!("permission {label} — takes effect on the next run")
            } else {
                format!("permission {label}")
            };
            app.notify(Notice::keyed("permission", text).immediate());
        }
        // The gate is "nothing is running", not "a run has finished": with no
        // run at all there is nothing to wait for, and the old status check
        // answered an idle `/commit` with "wait for the run to finish".
        Some(Action::CommitChanges) if !app.run_in_flight(session.as_ref()) => {
            if app.pending_changes(session.as_ref()) == 0 {
                app.notify(Notice::keyed("changeset", "nothing is staged").immediate());
                return Ok(());
            }
            if let Some(active) = session.as_ref() {
                // Oldest first: two turns that touched one file must land in the
                // order they were written, or the earlier edit wins.
                let mut total = 0;
                let mut failure = None;
                for change_set in &app.change_sets {
                    match active.sandbox.commit(change_set) {
                        Ok(count) => total += count,
                        Err(error) => failure = Some(error.to_string()),
                    }
                }
                match failure {
                    Some(error) => app
                        .notify(Notice::keyed("changeset", format!("commit failed: {error}")).error()),
                    None => app.notify(
                        Notice::keyed("changeset", format!("committed {total} entries")).immediate(),
                    ),
                }
            }
        }
        Some(Action::DiscardChanges) if !app.run_in_flight(session.as_ref()) => {
            if app.pending_changes(session.as_ref()) == 0 {
                app.notify(Notice::keyed("changeset", "nothing is staged").immediate());
                return Ok(());
            }
            if let Some(active) = session.as_ref() {
                let total: usize = app
                    .change_sets
                    .iter()
                    .map(|change_set| active.sandbox.discard(change_set))
                    .sum();
                app.notify(
                    Notice::keyed("changeset", format!("discarded {total} entries")).immediate(),
                );
            }
        }
        Some(Action::Quit) => {
            let staged = app.pending_changes(session.as_ref());
            if staged > 0 {
                app.notify(
                    Notice::keyed(
                        "changeset",
                        format!("{staged} staged entries: c to commit, d to discard"),
                    )
                    .immediate(),
                );
            } else {
                app.quit = true;
            }
        }
        // `?` is a character, so it only opens the sheet when there is no draft
        // to type it into.
        Some(Action::ReviewChanges) => {
            let changes = app.staged_changes(session.as_ref());
            let rows = surfaces::diff_rows(&changes, app.columns);
            app.overlay = Some(Overlay::open(Surface::Diff, rows));
            app.overlay_searching = false;
        }
        Some(Action::ShowStatus) => {
            let fields = app.status_fields(session.as_ref(), provider, args);
            let rows = surfaces::status_rows(&fields, app.columns);
            app.overlay = Some(Overlay::open(Surface::Status, rows));
            app.overlay_searching = false;
        }
        Some(Action::ToggleMouse) => {
            app.mouse_captured = !app.mouse_captured;
            app.mouse_dirty = true;
            app.notify(
                Notice::keyed(
                    "mouse",
                    if app.mouse_captured {
                        "the wheel scrolls the transcript"
                    } else {
                        "the wheel and drag-select belong to the terminal again"
                    },
                )
                .immediate(),
            );
        }
        Some(Action::Retry) => {
            match app.composer.history().last().cloned() {
                Some(last) => {
                    app.composer.set_draft(last);
                    app.notify(
                        Notice::keyed("retry", "last message restored — enter to send")
                            .immediate(),
                    );
                }
                None => app.notify(
                    Notice::keyed("retry", "nothing has been sent yet").immediate(),
                ),
            }
        }
        Some(Action::ExportTranscript) => {
            let path = app
                .command_argument
                .clone()
                .map(PathBuf::from)
                .unwrap_or_else(|| app.current_log.with_extension("md"));
            let body = app.export_markdown();
            match host_io::write_text(&path, &body) {
                Ok(()) => app.notify(
                    Notice::keyed("export", format!("wrote {}", path.display())).immediate(),
                ),
                Err(error) => {
                    app.notify(Notice::keyed("export", format!("export failed: {error}")).error().immediate());
                }
            }
        }
        Some(Action::ClearSession) => {
            if session.as_ref().is_some_and(|active| active.task.is_some()) {
                app.notify(
                    Notice::keyed("clear", "cancel the run in progress first").error().immediate(),
                );
                return Ok(());
            }
            let staged = app.pending_changes(session.as_ref());
            if staged > 0 {
                app.notify(
                    Notice::keyed(
                        "clear",
                        format!("{staged} staged entries: ctrl+s to commit or /discard first"),
                    )
                    .error(),
                );
                return Ok(());
            }
            // Everything derived from the old conversation goes with it.
            let next_log = next_log_path(&app.current_log, host_io::exists);
            match JsonlPersistence::open(&next_log) {
                Ok(opened) => {
                    app.state = AppState::default();
                    app.diffs.clear();
                    app.baselines.clear();
                    app.toggled.clear();
                    app.change_sets.clear();
                    app.scroll = None;
                    app.turn_started_ms = None;
                    app.cancel_armed = false;
                    app.current_log = next_log;
                    *persistence = Arc::new(opened);
                    *session = None;
                    app.notify(Notice::keyed("clear", "fresh conversation").immediate());
                }
                Err(error) => {
                    app.notify(Notice::keyed("clear", format!("cannot open a log: {error}")).error());
                }
            }
        }
        Some(Action::CommitChanges | Action::DiscardChanges) => app.notify(
            Notice::keyed("changeset", "the run is still going; wait for it to finish")
                .immediate()
                .for_ms(3_000),
        ),
        Some(Action::HelpOpen) if app.composer.is_empty() => app.open_surface(Surface::Help),
        Some(Action::PaletteOpen) => app.open_surface(Surface::Palette),
        Some(Action::BrowseLogs) => app.open_surface(Surface::Browser),
        Some(Action::TranscriptOpen) => app.open_surface(Surface::Transcript),
        Some(Action::EditorOpen) => match app
            .focused_location()
            .and_then(|entry| entry.locations.first().cloned())
        {
            Some(location) => app.pending_editor = Some(args.workspace.join(&location.path)),
            None => app.notify(
                Notice::keyed("editor", "no file location in view to open")
                    .immediate()
                    .for_ms(3_000),
            ),
        },
        Some(Action::CompletionAccept) => {
            if let Some(open) = app.completion.take() {
                if let Some(item) = open.current() {
                    let (draft, caret) = completion::apply(
                        app.composer.draft(),
                        app.composer.cursor(),
                        open.start,
                        item,
                    );
                    app.composer.set_draft(draft);
                    app.composer.set_cursor(caret);
                }
            }
        }
        // A character with no binding is what the user meant to type.
        _ => {
            if let Key::Char(ch) = chord.key {
                if !chord.ctrl && !chord.alt {
                    app.composer.insert(&ch.to_string());
                }
            }
        }
    }
    // The list is recomputed after every key rather than only after insertions:
    // a caret move out of the `@…` span has to close it, and a deletion back
    // into one has to open it.
    app.refresh_completion(&args.workspace);
    Ok(())
}

/// Applies one key while a full-screen surface is open.
async fn handle_overlay_key(
    app: &mut App,
    args: &Args,
    provider: &ProviderConfig,
    persistence: &mut Arc<JsonlPersistence>,
    session: &mut Option<Session>,
    chord: Chord,
    action: Option<Action>,
) {
    let Some(overlay) = app.overlay.as_mut() else {
        return;
    };
    let surface = overlay.surface;

    // The sheet is a sheet: it has nothing to take, so any key puts it away
    // rather than making the reader hunt for the one that does.
    if surface == Surface::Help {
        app.overlay = None;
        return;
    }

    match action {
        Some(Action::Cancel) => {
            app.overlay = None;
            cancel(app, session);
            return;
        }
        // `q` closes outright. Only Esc is two-step, and only because it is the
        // key a mistyped filter is undone with: clearing it there is a rescue,
        // whereas making `q` need two presses is just a key that did nothing.
        Some(Action::Close) if !app.overlay_searching => {
            app.overlay = None;
            return;
        }
        Some(Action::Escape) if !app.overlay_searching => {
            if !overlay.clear_query() {
                app.overlay = None;
            }
            return;
        }
        Some(Action::Escape) => {
            overlay.clear_query();
            app.overlay_searching = false;
            return;
        }
        Some(Action::ListPrevious) | Some(Action::ScrollUp) => {
            overlay.move_by(-1);
            return;
        }
        Some(Action::ListNext) | Some(Action::ScrollDown) => {
            overlay.move_by(1);
            return;
        }
        Some(Action::ScrollPageUp) => {
            overlay.move_by(-10);
            return;
        }
        Some(Action::ScrollPageDown) => {
            overlay.move_by(10);
            return;
        }
        Some(Action::SearchStart) if surface == Surface::Transcript => {
            app.overlay_searching = true;
            return;
        }
        Some(Action::SearchNext) if !app.overlay_searching => {
            overlay.jump_to_match(true);
            return;
        }
        Some(Action::SearchPrevious) if !app.overlay_searching => {
            overlay.jump_to_match(false);
            return;
        }
        Some(Action::RestoreDraft) if !app.overlay_searching => {
            if let Some(text) = overlay.selected_value() {
                app.composer.set_draft(text.trim());
                app.overlay = None;
                app.notify(Notice::keyed("restore", "row restored to the draft").immediate());
            }
            return;
        }
        Some(Action::ListAccept) => {
            if app.overlay_searching {
                app.overlay_searching = false;
                return;
            }
            let taken = overlay.selected_value();
            app.overlay = None;
            if let Some(value) = taken {
                accept_surface(app, args, provider, persistence, session, surface, &value).await;
            }
            return;
        }
        _ => {}
    }

    // Anything else is typing into the box. A searching surface only takes
    // typing while the search is open, so `n` and `q` keep working.
    match chord.key {
        Key::Backspace => {
            if !overlay.pop_query() && !app.overlay_searching {
                app.overlay = None;
            }
        }
        // A searching surface only takes typing while the search is open, so
        // `n` and `q` keep working over the transcript.
        Key::Char(ch)
            if !chord.ctrl && !chord.alt && (!surface.searches() || app.overlay_searching) =>
        {
            overlay.push_query(ch);
        }
        _ => {}
    }
}

/// Does whatever taking a row on `surface` means.
async fn accept_surface(
    app: &mut App,
    args: &Args,
    provider: &ProviderConfig,
    persistence: &mut Arc<JsonlPersistence>,
    session: &mut Option<Session>,
    surface: Surface,
    value: &str,
) {
    match surface {
        Surface::Palette => {
            let Some(command) = surfaces::command(value) else {
                return;
            };
            let Some(action) = command.action else { return };
            // The palette runs the same action the key would, rather than a
            // second implementation of it that can drift.
            let context = app.context();
            let _ = Box::pin(handle_key(
                app,
                args,
                provider,
                persistence,
                session,
                context,
                Chord::plain(Key::Escape),
                Some(action),
            ))
            .await;
        }
        Surface::Browser => open_log(app, args, provider, persistence, session, Path::new(value)).await,
        // Taking a row in the diff opens that file; the sheet and the status
        // summary have nothing to take at all.
        Surface::Diff => app.pending_editor = Some(args.workspace.join(value)),
        Surface::Help | Surface::Status | Surface::Transcript => {}
    }
}

/// Replays another durable log and resumes it if it has not finished.
///
/// A run in flight is never displaced: its events would keep arriving into a
/// projection that is no longer about it.
async fn open_log(
    app: &mut App,
    args: &Args,
    provider: &ProviderConfig,
    persistence: &mut Arc<JsonlPersistence>,
    session: &mut Option<Session>,
    path: &Path,
) {
    if session.as_ref().is_some_and(|active| active.task.is_some()) {
        app.notify(
            Notice::keyed("browser", "finish or cancel this run before opening another log")
                .error(),
        );
        return;
    }
    if path == app.current_log {
        app.notify(Notice::keyed("browser", "that log is already open").immediate());
        return;
    }
    let opened = match JsonlPersistence::open(path) {
        Ok(opened) => Arc::new(opened),
        Err(error) => {
            app.notify(Notice::keyed("browser", format!("cannot open log: {error}")).error());
            return;
        }
    };
    let (recovery, state) = match load_recovery(&opened) {
        Ok(loaded) => loaded,
        Err(error) => {
            app.notify(Notice::keyed("browser", format!("cannot replay log: {error}")).error());
            return;
        }
    };
    // Everything derived from the old log goes with it. A stale diff or fold
    // would be attributed to a call this log never made.
    app.state = state;
    app.diffs.clear();
    app.baselines.clear();
    app.toggled.clear();
    app.scroll = None;
    app.current_log = path.to_path_buf();
    *persistence = opened;
    *session = None;
    let events = recovery.events.len();
    if app.state.status.terminal() {
        app.notify(
            Notice::keyed(
                "browser",
                format!("replayed {events} events; that run had already finished"),
            )
            .with_priority(Priority::High),
        );
        return;
    }
    match start_session(
        args,
        &app.permission,
        provider,
        persistence.clone(),
        Origin::Resume(recovery),
        None,
        None,
    )
    .await
    {
        Ok(started) => {
            app.cancel_armed = false;
            app.turn_started_ms = Some(host_io::now_ms());
            app.track_change_set(&started.change_set);
            *session = Some(started);
            app.notify(
                Notice::keyed("browser", format!("resumed from {events} events"))
                    .with_priority(Priority::High),
            );
        }
        Err(error) => {
            app.state.status = RunStatus::NeedsAction;
            app.notify(Notice::keyed("browser", error).error());
        }
    }
}

/// Sends the decision on the selected row.
fn answer_approval(app: &mut App, index: usize) {
    let Some(pending) = app.approval.take() else {
        return;
    };
    let option = pending
        .options
        .get(index)
        .cloned()
        .unwrap_or_else(|| pending.options[approval::fail_closed_index(&pending.options)].clone());
    let answer = if option.allowed {
        ApprovalAnswer::AllowOnce {
            user_id: "dev-tui-user".into(),
        }
    } else {
        ApprovalAnswer::Reject {
            user_id: "dev-tui-user".into(),
        }
    };
    if pending.prompt.respond_to.send(answer).is_ok() {
        app.state.status = RunStatus::Running;
        app.notify(Notice::keyed("approval", option.label.clone()).immediate());
    } else {
        app.notify(Notice::keyed("approval", "approval is no longer active").error());
    }
    if let Some(mode) = option.then_mode {
        app.permission = mode;
        app.notify(
            Notice::keyed(
                "permission",
                format!(
                    "permission {} — takes effect on the next run",
                    approval::mode_label(&app.permission)
                ),
            )
            .with_priority(Priority::High),
        );
    }
    if option.accepts_feedback {
        app.notify(
            Notice::keyed(
                "feedback",
                "say why — your next message goes to the model as the reason",
            )
            .with_priority(Priority::High),
        );
    }
}

/// Two-step cancellation, then a bounded exit.
fn cancel(app: &mut App, session: &Option<Session>) {
    match session {
        Some(active) if active.task.is_some() => {
            if app.cancel_armed {
                app.quit = true;
            } else {
                app.cancel_armed = true;
                active.handle.cancel();
                app.notify(
                    Notice::keyed("cancel", "cancellation requested; press again to force quit")
                        .with_priority(Priority::High),
                );
            }
        }
        _ => app.quit = true,
    }
}

/// Steers a live run, or starts a new one.
async fn submit_text(
    app: &mut App,
    args: &Args,
    provider: &ProviderConfig,
    persistence: &mut Arc<JsonlPersistence>,
    session: &mut Option<Session>,
    text: String,
) {
    match session {
        Some(active) if active.task.is_some() => {
            if let Err(error) = active
                .handle
                .submit(UserInput::Message(vec![ContentBlock::text(text)]))
                .await
            {
                app.notify(Notice::keyed("input", format!("input rejected: {error}")).error());
            } else {
                app.notify(
                    Notice::keyed("input", "queued for the current step")
                        .immediate()
                        .for_ms(3_000),
                );
            }
        }
        // The run has finished, so this message is a new turn. A Run ends when
        // its inbox drains — that is the kernel's unit — so continuing the
        // conversation means forking the finished Run into a new one that
        // carries its history. The transcript on screen is the session, and it
        // just keeps growing.
        Some(active) => {
            let source = match app.state.run_id.clone() {
                Some(run_id) => run_id,
                None => {
                    app.notify(Notice::keyed("input", "no run to continue").error());
                    return;
                }
            };
            let events = match persistence.load_events() {
                Ok(events) => events,
                Err(error) => {
                    app.notify(
                        Notice::keyed("input", format!("cannot read the durable log: {error}"))
                            .error(),
                    );
                    return;
                }
            };
            let reuse = Some(active.sandbox.clone());
            // Each Run owns its own log: two runs in one file would share a
            // sequence counter and make `--resume` read one run's history as
            // another's.
            let next_log = next_log_path(&app.current_log, host_io::exists);
            let opened = match JsonlPersistence::open(&next_log) {
                Ok(opened) => Arc::new(opened),
                Err(error) => {
                    app.notify(
                        Notice::keyed("input", format!("cannot open the next log: {error}")).error(),
                    );
                    return;
                }
            };
            match start_session(
                args,
                &app.permission,
                provider,
                opened.clone(),
                Origin::Continue { source, events },
                reuse,
                Some(&text),
            )
            .await
            {
                Ok(started) => {
                    app.cancel_armed = false;
                    app.turn_started_ms = Some(host_io::now_ms());
                    app.current_log = next_log;
                    app.track_change_set(&started.change_set);
                    *persistence = opened;
                    *session = Some(started);
                }
                Err(error) => app.notify(Notice::keyed("start", error).error()),
            }
        }
        None => {
            match start_session(
                args,
                &app.permission,
                provider,
                persistence.clone(),
                Origin::Fresh,
                None,
                Some(&text),
            )
            .await
            {
                Ok(started) => {
                    app.cancel_armed = false;
                    app.turn_started_ms = Some(host_io::now_ms());
                    app.track_change_set(&started.change_set);
                    *session = Some(started);
                }
                Err(error) => app.notify(Notice::keyed("start", error).error()),
            }
        }
    }
}

/// The next free `<stem>-N.jsonl` beside `log`.
///
/// A turn is a Run and a Run is a log, so a conversation leaves a numbered
/// trail rather than one file whose history belongs to several runs at once.
///
/// `taken` is the only thing here that touches the filesystem, and it is a
/// parameter so the numbering can be tested without one.
fn next_log_path(log: &Path, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let dir = log.parent().unwrap_or(Path::new("."));
    let stem = log
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "run".into());
    // `a-2` continues as `a-3`, not as `a-2-2`.
    let base = match stem.rsplit_once('-') {
        Some((head, tail)) if tail.chars().all(|ch| ch.is_ascii_digit()) && !tail.is_empty() => {
            head.to_string()
        }
        _ => stem,
    };
    for turn in 2..10_000 {
        let candidate = dir.join(format!("{base}-{turn}.jsonl"));
        if !taken(&candidate) {
            return candidate;
        }
    }
    dir.join(format!("{base}-overflow.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_turn_gets_its_own_log_beside_the_last() {
        // A Run is a log. Two runs in one file would share a sequence counter,
        // and `--resume` would read one run's history as another's.
        let dir = PathBuf::from("/chat");
        let first = dir.join("chat.jsonl");
        let none = |_: &Path| false;
        assert_eq!(next_log_path(&first, none), dir.join("chat-2.jsonl"));

        let second_taken = |path: &Path| path == dir.join("chat-2.jsonl");
        // The third turn continues the numbering rather than nesting suffixes,
        // whichever log it is asked from.
        assert_eq!(next_log_path(&first, second_taken), dir.join("chat-3.jsonl"));
        assert_eq!(
            next_log_path(&dir.join("chat-2.jsonl"), second_taken),
            dir.join("chat-3.jsonl")
        );
    }

    #[test]
    fn permission_presets_accept_both_names() {
        assert_eq!(parse_permission("plan").unwrap(), PermissionMode::Plan);
        assert_eq!(parse_permission("read-only").unwrap(), PermissionMode::Plan);
        assert_eq!(parse_permission("workspace-write").unwrap(), PermissionMode::Default);
        assert!(matches!(
            parse_permission("danger-full-access").unwrap(),
            PermissionMode::Accepted { .. }
        ));
        // An unknown preset is refused before the terminal enters raw mode.
        assert!(parse_permission("whatever").is_err());
    }
}
