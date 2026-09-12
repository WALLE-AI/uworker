use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use agentrs_config::config::Config;
use agentrs_config::todo::TodoMode;
use agentrs_protocol::events::SubAgentEventStatus;
use agentrs_protocol::events::Usage;
use agentrs_providers::LlmProvider;
use agentrs_tools::Tool;
use agentrs_tools::context::ToolContext;
use agentrs_tools::edit::EditTool;
use agentrs_tools::exec_command::ExecCommandTool;
use agentrs_tools::glob::GlobTool;
use agentrs_tools::grep::GrepTool;
use agentrs_tools::read::ReadTool;
use agentrs_tools::registry::ToolRegistry;
use agentrs_tools::task::{TaskCreateTool, TaskGetTool, TaskListTool, TaskStore, TaskUpdateTool, task_dir};
use agentrs_tools::team::SendMessageTool;
use agentrs_tools::todo::{TodoStore, TodoWriteTool};
use agentrs_tools::tool_search::ToolSearchTool;
use agentrs_tools::write::WriteTool;
use agentrs_types::message::TokenUsage;
use agentrs_types::subagent::{
    ForkOverrides, Spawner, SubAgentId, SubAgentIsolation, SubAgentResult, SubAgentSpec, SubAgentStatus,
};
use agentrs_types::team::{AgentName, TeamRuntime};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::context::{SubAgentIdentity, append_subagent_role, build_child_prompt};
use super::definitions::AgentDefinitions;
use super::handle::SubAgentHandle;
use super::output::SubAgentSink;
use super::registry::SubAgentRegistry;
use super::worktree::{Cleanup as WorktreeCleanup, Worktree};
use crate::engine::{AgentEngine, AgentResult};
use crate::error::AgentError;
use crate::output::OutputSink;
use crate::output::null_sink::NullSink;
use crate::team::{InProcessTeamRuntime, TeammateInbox, render_teammate_messages};
use crate::todo_reminder::{PlanSource, TodoRuntime};
use crate::tool_policy::ToolPolicy;

#[derive(Clone)]
pub struct AgentSpawner {
    provider: Arc<dyn LlmProvider>,
    base_config: Config,
    cwd: PathBuf,
    runtime_env: Vec<(String, String)>,
    tool_policy: ToolPolicy,
    registry: Arc<SubAgentRegistry>,
    progress_output: Arc<dyn OutputSink>,
    parent_session_id: Arc<RwLock<Option<String>>>,
    parent_tools: Vec<Arc<dyn Tool>>,
    definitions: Arc<AgentDefinitions>,
    team_runtime: Option<Arc<InProcessTeamRuntime>>,
}

impl AgentSpawner {
    pub fn new(provider: Arc<dyn LlmProvider>, config: Config, cwd: PathBuf, tool_policy: ToolPolicy) -> Self {
        Self::new_with_env(provider, config, cwd, Vec::new(), tool_policy)
    }

    pub fn new_with_env(
        provider: Arc<dyn LlmProvider>,
        config: Config,
        cwd: PathBuf,
        runtime_env: Vec<(String, String)>,
        tool_policy: ToolPolicy,
    ) -> Self {
        let definitions = Arc::new(AgentDefinitions::load(&cwd, config.subagent.builtin_agents));
        let registry = Arc::new(SubAgentRegistry::new(
            config.subagent.max_concurrent,
            config.subagent.turn_output_budget,
            config.subagent.cancel_grace,
        ));
        Self {
            provider,
            base_config: config,
            cwd,
            runtime_env,
            tool_policy,
            registry,
            progress_output: Arc::new(NullSink),
            parent_session_id: Arc::new(RwLock::new(None)),
            parent_tools: Vec::new(),
            definitions,
            team_runtime: None,
        }
    }

    pub(crate) fn with_progress_output(mut self, output: Arc<dyn OutputSink>) -> Self {
        self.progress_output = output;
        self
    }

    pub(crate) fn with_parent_tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.parent_tools = tools;
        self
    }

    pub(crate) fn with_team_runtime(mut self, runtime: Arc<InProcessTeamRuntime>) -> Self {
        self.team_runtime = Some(runtime);
        self
    }

    pub async fn spawn_one(&self, spec: SubAgentSpec) -> SubAgentResult {
        self.spawn(spec, ForkOverrides::default(), CancellationToken::new())
            .await
    }

    pub async fn spawn_parallel(&self, specs: Vec<SubAgentSpec>, cancel: CancellationToken) -> Vec<SubAgentResult> {
        futures::future::join_all(
            specs
                .into_iter()
                .map(|spec| self.spawn(spec, ForkOverrides::default(), cancel.child_token())),
        )
        .await
    }

    pub(crate) async fn spawn_persistent(&self, spec: SubAgentSpec, cancel: CancellationToken) -> SubAgentResult {
        if spec.resume.is_some() {
            return failed_result(
                spec,
                "A persistent team member cannot resume an unrelated task id".to_string(),
            );
        }
        if spec.isolation == SubAgentIsolation::Worktree {
            return failed_result(
                spec,
                "Persistent team members currently require shared isolation".to_string(),
            );
        }
        if spec.depth >= self.base_config.subagent.depth {
            return failed_result(
                spec,
                format!(
                    "Sub-agent depth exceeds subagent.depth={}",
                    self.base_config.subagent.depth
                ),
            );
        }
        let Some(team_runtime) = self.team_runtime.clone() else {
            return failed_result(
                spec,
                "Team mode is disabled; enable [team] before spawning a persistent member".to_string(),
            );
        };
        let id = SubAgentId::new(uuid::Uuid::now_v7().to_string());
        let status = Arc::new(RwLock::new(SubAgentStatus::Pending));
        let task_status = Arc::clone(&status);
        let task_cancel = cancel.child_token();
        let handle_cancel = task_cancel.clone();
        let task_name = spec.name.clone();
        let (member_name, _, inbox) = match team_runtime.register_member(
            &spec.name,
            spec.agent_type.clone(),
            id.clone(),
            handle_cancel.clone(),
        ) {
            Ok(member) => member,
            Err(error) => return failed_result_with_id(id, spec.name, error.to_string()),
        };
        let permits = self.registry.permits();
        let spawner = self.clone();
        let task_id = id.clone();
        let result_name = task_name.clone();
        let join = tokio::spawn(async move {
            let _member_exit = MemberExitGuard {
                runtime: team_runtime,
                name: member_name,
            };
            let mut next = spec;
            let mut usage = TokenUsage::default();
            let mut turns = 0;
            let final_result = loop {
                let permit = tokio::select! {
                    permit = permits.clone().acquire_owned() => permit.ok(),
                    _ = task_cancel.cancelled() => None,
                };
                let Some(permit) = permit else {
                    break cancelled_result(task_id.clone(), result_name.clone());
                };
                set_status(&task_status, SubAgentStatus::Running);
                let result = spawner
                    .run_child(
                        task_id.clone(),
                        next.clone(),
                        ForkOverrides::default(),
                        task_cancel.clone(),
                    )
                    .await;
                drop(permit);
                add_usage(&mut usage, &result.usage);
                turns += result.turns;
                if result.status.is_error() {
                    break SubAgentResult { usage, turns, ..result };
                }
                set_status(&task_status, SubAgentStatus::Idle);
                let messages = tokio::select! {
                    _ = task_cancel.cancelled() => Vec::new(),
                    messages = inbox.wait() => messages,
                };
                if messages.is_empty() {
                    break cancelled_result(task_id.clone(), result_name.clone());
                }
                next.prompt = render_teammate_messages(&messages);
                next.resume = Some(task_id.clone());
            };
            set_status(&task_status, final_result.status);
            final_result
        });
        self.registry.register(SubAgentHandle::new_persistent(
            id.clone(),
            task_name.clone(),
            status,
            handle_cancel,
            join,
        ));
        SubAgentResult {
            id,
            name: task_name,
            text: "Persistent team member started and is processing its initial task".to_string(),
            usage: TokenUsage::default(),
            turns: 0,
            status: SubAgentStatus::Running,
        }
    }

    pub(crate) fn registry(&self) -> Arc<SubAgentRegistry> {
        Arc::clone(&self.registry)
    }

    pub(crate) fn parent_session_slot(&self) -> Arc<RwLock<Option<String>>> {
        Arc::clone(&self.parent_session_id)
    }

    pub(crate) fn max_per_call(&self) -> usize {
        self.base_config.subagent.max_per_call
    }

    async fn run_child(
        &self,
        id: SubAgentId,
        spec: SubAgentSpec,
        overrides: ForkOverrides,
        cancel: CancellationToken,
    ) -> SubAgentResult {
        let worktree = if spec.isolation == SubAgentIsolation::Worktree {
            match Worktree::create(&self.cwd, &id).await {
                Ok(worktree) => Some(worktree),
                Err(error) => {
                    return failed_result_with_id(
                        id,
                        spec.name,
                        format!("Unable to create isolated worktree: {error}"),
                    );
                }
            }
        } else {
            None
        };
        let child_cwd = worktree
            .as_ref()
            .map(|worktree| worktree.path().to_path_buf())
            .unwrap_or_else(|| self.cwd.clone());
        let team_context = if spec.persistent {
            match persistent_context(self.team_runtime.as_deref(), &spec.name) {
                Ok(context) => Some(context),
                Err(error) => return failed_result_with_id(id, spec.name, error),
            }
        } else {
            None
        };
        let mut config = self.base_config.clone();
        let Some(definition) = self.definitions.resolve(spec.agent_type.as_deref()).cloned() else {
            return failed_result_with_id(
                id,
                spec.name,
                "Unknown sub-agent type; choose a configured sub-agent name".to_string(),
            );
        };
        config.max_turns = Some(
            spec.max_turns
                .or(definition.max_turns)
                .unwrap_or(config.subagent.max_turns),
        );
        config.max_tokens = Some(
            spec.max_tokens
                .or(definition.max_tokens)
                .unwrap_or(config.subagent.max_tokens),
        );
        config.session.enabled = config.subagent.persist_sessions;
        if let Some(model) = overrides.model.or(definition.model.clone()) {
            config.model = model;
        }

        let reminder_turns = config.todo.reminder_turns;
        let requested_tools = if overrides.allowed_tools.is_empty() {
            &definition.allowed_tools
        } else {
            &overrides.allowed_tools
        };
        let child_policy = definition_policy(
            &self.tool_policy,
            requested_tools,
            &definition.denied_tools,
            &self.parent_tools,
        );
        config.system_prompt = match spec.system_prompt.clone().or(definition.system_prompt.clone()) {
            Some(mut prompt) => {
                append_subagent_role(&mut prompt, team_context.as_ref().map(|context| &context.identity));
                Some(prompt)
            }
            None => match build_child_prompt(
                &child_policy,
                &config,
                &child_cwd,
                team_context.as_ref().map(|context| &context.identity),
                definition.omit_project_rules,
            ) {
                Ok(prompt) => Some(prompt),
                Err(error) => {
                    return SubAgentResult {
                        id,
                        name: spec.name,
                        text: format!("Unable to build sub-agent context: {error}"),
                        usage: TokenUsage::default(),
                        turns: 0,
                        status: SubAgentStatus::Failed,
                    };
                }
            },
        };
        let (mut tools, plan_source) = project_tools(
            &self.parent_tools,
            &child_policy,
            &config,
            &child_cwd,
            &self.runtime_env,
            definition.name == "explore",
        );
        if let Some(context) = &team_context
            && child_policy.allows("SendMessage")
        {
            tools.register(Box::new(SendMessageTool::new(Arc::new(context.runtime.clone()))));
        }
        if spec.depth + 1 < config.subagent.depth && child_policy.allows("Spawn") {
            tools.register(Box::new(crate::spawn_tool::SpawnTool::for_depth(
                Arc::new(self.clone()),
                spec.depth + 1,
            )));
        }
        if child_policy.allows("ToolSearch") {
            let visible = tools.to_tool_defs();
            tools.register(Box::new(ToolSearchTool::new(visible)));
        }
        self.progress_output
            .emit_subagent_started(id.as_str(), &spec.name, "", spec.depth);
        let output: Arc<dyn OutputSink> = Arc::new(SubAgentSink::new(
            Arc::clone(&self.progress_output),
            id.as_str().to_string(),
        ));
        let provider_label = config.provider_label.clone();
        let resumed = if let Some(resume) = spec.resume.as_ref() {
            match crate::session::SessionManager::new(
                config.session.directory.clone().into(),
                config.session.max_sessions,
            )
            .load(resume.as_str())
            {
                Ok(session) => Some(session),
                Err(error) => {
                    let resume_id = resume.to_string();
                    return failed_result_with_id(
                        id,
                        spec.name,
                        format!("Unable to resume sub-agent {resume_id}: {error}"),
                    );
                }
            }
        } else {
            None
        };
        let baseline_usage = resumed
            .as_ref()
            .map(|session| session.total_usage.clone())
            .unwrap_or_default();
        let mut engine = if let Some(session) = resumed {
            AgentEngine::resume_with_provider_and_env(
                Arc::clone(&self.provider),
                config,
                tools,
                output,
                session,
                child_cwd.clone(),
                self.runtime_env.clone(),
            )
        } else {
            AgentEngine::new_with_provider_and_env(
                Arc::clone(&self.provider),
                config,
                tools,
                output,
                child_cwd.clone(),
                self.runtime_env.clone(),
            )
        };
        if spec.resume.is_none()
            && let Some(parent_id) = self
                .parent_session_id
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
            && let Err(error) = engine.init_child_session(
                &parent_id,
                &provider_label,
                &child_cwd.to_string_lossy(),
                Some(id.as_str()),
            )
        {
            return SubAgentResult {
                id,
                name: spec.name,
                text: format!("Unable to create sub-agent session: {error}"),
                usage: TokenUsage::default(),
                turns: 0,
                status: SubAgentStatus::Failed,
            };
        }
        engine.set_initial_reasoning_effort(overrides.effort.or(definition.effort));
        engine.set_temperature(definition.temperature);
        engine.set_tool_policy(child_policy);
        if let Some(context) = &team_context {
            engine.set_team_inbox(Arc::clone(&context.inbox));
        }
        if let Some(source) = plan_source {
            engine.set_todo_runtime(TodoRuntime::new(source, reminder_turns));
        }

        tracing::info!(target: "agentrs_agent", subagent_id = %id, depth = spec.depth, "sub-agent started");
        enum Outcome {
            Run(Result<AgentResult, AgentError>),
            Cancelled,
        }
        let outcome = tokio::select! {
            result = engine.run(&spec.prompt, "") => Outcome::Run(result),
            _ = cancel.cancelled() => Outcome::Cancelled,
        };
        let mut result = match outcome {
            Outcome::Run(Ok(result)) => SubAgentResult {
                id,
                name: spec.name,
                text: result.text,
                usage: usage_delta(&result.usage, &baseline_usage),
                turns: result.turns,
                status: if spec.persistent {
                    SubAgentStatus::Idle
                } else {
                    SubAgentStatus::Finished
                },
            },
            Outcome::Run(Err(error)) => SubAgentResult {
                id,
                name: spec.name,
                text: format!("Sub-agent error: {error}"),
                usage: TokenUsage::default(),
                turns: 0,
                status: SubAgentStatus::Failed,
            },
            Outcome::Cancelled => {
                engine.cancel_running_tools();
                SubAgentResult {
                    id,
                    name: spec.name,
                    text: "Sub-agent cancelled".to_string(),
                    usage: TokenUsage::default(),
                    turns: 0,
                    status: SubAgentStatus::Cancelled,
                }
            }
        };
        if let Some(worktree) = worktree {
            match worktree.cleanup_if_clean().await {
                Ok(WorktreeCleanup::Removed) => {}
                Ok(WorktreeCleanup::Preserved(path)) => {
                    result
                        .text
                        .push_str(&format!("\n\nIsolated worktree preserved at: {}", path.display()));
                }
                Err(error) => {
                    tracing::warn!(target: "agentrs_agent", %error, "unable to clean isolated worktree");
                    result.text.push_str(&format!("\n\nWorktree cleanup failed: {error}"));
                }
            }
        }
        tracing::info!(target: "agentrs_agent", subagent_id = %result.id, status = ?result.status, "sub-agent stopped");
        self.progress_output.emit_subagent_finished(
            result.id.as_str(),
            protocol_status(result.status),
            result.turns,
            Usage {
                input_tokens: result.usage.input_tokens,
                output_tokens: result.usage.output_tokens,
                cache_read_tokens: (result.usage.cache_read_tokens > 0).then_some(result.usage.cache_read_tokens),
                cache_write_tokens: (result.usage.cache_creation_tokens > 0)
                    .then_some(result.usage.cache_creation_tokens),
            },
        );
        result
    }
}

struct PersistentContext {
    identity: SubAgentIdentity,
    runtime: InProcessTeamRuntime,
    inbox: Arc<TeammateInbox>,
}

struct MemberExitGuard {
    runtime: Arc<InProcessTeamRuntime>,
    name: AgentName,
}

impl Drop for MemberExitGuard {
    fn drop(&mut self) {
        self.runtime.member_exited(&self.name);
    }
}

fn persistent_context(runtime: Option<&InProcessTeamRuntime>, raw_name: &str) -> Result<PersistentContext, String> {
    let runtime = runtime.ok_or_else(|| "Team mode is disabled".to_string())?;
    let name = AgentName::new(raw_name).map_err(|error| error.to_string())?;
    let team = runtime
        .current_team()
        .ok_or_else(|| "No active team; call TeamCreate first".to_string())?;
    let actor = runtime.for_member(&name).map_err(|error| error.to_string())?;
    let inbox = runtime
        .inbox_for(&name)
        .ok_or_else(|| format!("Team member '{name}' has no inbox"))?;
    let teammates = runtime
        .members()
        .into_iter()
        .filter(|member| member.name != name)
        .map(|member| member.name.to_string())
        .collect();
    Ok(PersistentContext {
        identity: SubAgentIdentity {
            name: name.to_string(),
            team: Some(team.id.to_string()),
            teammates,
        },
        runtime: actor,
        inbox,
    })
}

fn add_usage(total: &mut TokenUsage, additional: &TokenUsage) {
    total.input_tokens += additional.input_tokens;
    total.output_tokens += additional.output_tokens;
    total.cache_creation_tokens += additional.cache_creation_tokens;
    total.cache_read_tokens += additional.cache_read_tokens;
}

fn protocol_status(status: SubAgentStatus) -> SubAgentEventStatus {
    match status {
        SubAgentStatus::Pending => SubAgentEventStatus::Pending,
        SubAgentStatus::Running => SubAgentEventStatus::Running,
        SubAgentStatus::Idle => SubAgentEventStatus::Idle,
        SubAgentStatus::Finished => SubAgentEventStatus::Finished,
        SubAgentStatus::Failed => SubAgentEventStatus::Failed,
        SubAgentStatus::Cancelled => SubAgentEventStatus::Cancelled,
    }
}

fn usage_delta(total: &TokenUsage, baseline: &TokenUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: total.input_tokens.saturating_sub(baseline.input_tokens),
        output_tokens: total.output_tokens.saturating_sub(baseline.output_tokens),
        cache_creation_tokens: total
            .cache_creation_tokens
            .saturating_sub(baseline.cache_creation_tokens),
        cache_read_tokens: total.cache_read_tokens.saturating_sub(baseline.cache_read_tokens),
    }
}

#[async_trait]
impl Spawner for AgentSpawner {
    async fn spawn(&self, spec: SubAgentSpec, overrides: ForkOverrides, cancel: CancellationToken) -> SubAgentResult {
        if spec.resume.is_some() && spec.isolation == SubAgentIsolation::Worktree {
            return failed_result(spec, "A resumed sub-agent cannot request a new worktree".to_string());
        }
        if spec.depth >= self.base_config.subagent.depth {
            return failed_result(
                spec,
                format!(
                    "Sub-agent depth exceeds subagent.depth={}; increase that configuration value to allow nesting",
                    self.base_config.subagent.depth
                ),
            );
        }
        if !self.registry.budget_available() {
            return failed_result(
                spec,
                "Sub-agent output budget exhausted; increase subagent.turn_output_budget".to_string(),
            );
        }
        let id = spec
            .resume
            .clone()
            .unwrap_or_else(|| SubAgentId::new(uuid::Uuid::now_v7().to_string()));
        let status = Arc::new(RwLock::new(SubAgentStatus::Pending));
        let task_status = Arc::clone(&status);
        let task_cancel = cancel.child_token();
        let handle_cancel = task_cancel.clone();
        let permits = self.registry.permits();
        let spawner = self.clone();
        let task_id = id.clone();
        let task_spec = spec.clone();
        let join = tokio::spawn(async move {
            let permit = tokio::select! {
                permit = permits.acquire_owned() => permit.ok(),
                _ = task_cancel.cancelled() => None,
            };
            if permit.is_none() {
                set_status(&task_status, SubAgentStatus::Cancelled);
                return cancelled_result(task_id, task_spec.name);
            }
            set_status(&task_status, SubAgentStatus::Running);
            let result = spawner.run_child(task_id, task_spec, overrides, task_cancel).await;
            set_status(&task_status, result.status);
            result
        });
        self.registry.register(SubAgentHandle::new(
            id.clone(),
            spec.name.clone(),
            status,
            handle_cancel,
            join,
        ));
        self.registry.wait(&id).await.unwrap_or_else(|| {
            failed_result_with_id(id, spec.name, "Sub-agent task ended without a result".to_string())
        })
    }
}

fn set_status(status: &RwLock<SubAgentStatus>, next: SubAgentStatus) {
    SubAgentHandle::transition_status(status, next);
}

fn cancelled_result(id: SubAgentId, name: String) -> SubAgentResult {
    SubAgentResult {
        id,
        name,
        text: "Sub-agent cancelled".to_string(),
        usage: TokenUsage::default(),
        turns: 0,
        status: SubAgentStatus::Cancelled,
    }
}

fn failed_result(spec: SubAgentSpec, text: String) -> SubAgentResult {
    let id = spec
        .resume
        .unwrap_or_else(|| SubAgentId::new(uuid::Uuid::now_v7().to_string()));
    failed_result_with_id(id, spec.name, text)
}

fn failed_result_with_id(id: SubAgentId, name: String, text: String) -> SubAgentResult {
    SubAgentResult {
        id,
        name,
        text,
        usage: TokenUsage::default(),
        turns: 0,
        status: SubAgentStatus::Failed,
    }
}

#[cfg(test)]
pub(crate) fn child_policy(parent: &ToolPolicy, allowed_tools: &[String]) -> ToolPolicy {
    if allowed_tools.is_empty() {
        return parent.clone();
    }
    ToolPolicy::allow_only(allowed_tools.iter().filter(|name| parent.allows(name)).cloned())
}

#[cfg(test)]
pub(crate) fn build_tool_registry(
    policy: &ToolPolicy,
    config: &Config,
    cwd: &Path,
    runtime_env: &[(String, String)],
) -> (ToolRegistry, Option<PlanSource>) {
    build_tool_registry_mode(policy, config, cwd, runtime_env, false)
}

fn build_tool_registry_mode(
    policy: &ToolPolicy,
    config: &Config,
    cwd: &Path,
    runtime_env: &[(String, String)],
    read_only_exec: bool,
) -> (ToolRegistry, Option<PlanSource>) {
    let exec: Box<dyn Tool> = if read_only_exec {
        Box::new(ExecCommandTool::new_read_only_with_env(
            cwd.to_path_buf(),
            runtime_env.to_vec(),
        ))
    } else {
        Box::new(ExecCommandTool::new_with_env(cwd.to_path_buf(), runtime_env.to_vec()))
    };
    let tool_context = ToolContext::new(config.file_cache.enabled.then(|| {
        Arc::new(RwLock::new(agentrs_tools::file_cache::FileStateCache::new(
            &config.file_cache,
        )))
    }));
    let tools: Vec<(&str, Box<dyn agentrs_tools::Tool>)> = vec![
        ("Read", Box::new(ReadTool::with_context(tool_context.clone()))),
        ("Write", Box::new(WriteTool::with_context(tool_context.clone()))),
        ("Edit", Box::new(EditTool::with_context(tool_context))),
        ("ExecCommand", exec),
        ("Grep", Box::new(GrepTool::new(cwd.to_path_buf()))),
        ("Glob", Box::new(GlobTool::new(cwd.to_path_buf()))),
    ];
    let mut registry = ToolRegistry::new();
    for (name, tool) in tools {
        if policy.allows(name) {
            registry.register(tool);
        }
    }
    let source = build_task_tracking(&mut registry, policy, config, cwd);
    (registry, source)
}

const REBUILT_FOR_CHILD: &[&str] = &[
    "Read",
    "Write",
    "Edit",
    "ExecCommand",
    "Grep",
    "Glob",
    "TodoWrite",
    "TaskCreate",
    "TaskList",
    "TaskGet",
    "TaskUpdate",
    "ToolSearch",
    "SendMessage",
];

const NEVER_INHERITED: &[&str] = &[
    "Skill",
    "Spawn",
    "EnterPlanMode",
    "ExitPlanMode",
    "TeamCreate",
    "TeamDelete",
    "SendMessage",
];

pub(crate) fn project_tools(
    parent_tools: &[Arc<dyn Tool>],
    policy: &ToolPolicy,
    config: &Config,
    cwd: &Path,
    runtime_env: &[(String, String)],
    read_only_exec: bool,
) -> (ToolRegistry, Option<PlanSource>) {
    let (mut registry, plan_source) = build_tool_registry_mode(policy, config, cwd, runtime_env, read_only_exec);
    for tool in parent_tools {
        let name = tool.name();
        if !policy.allows(name)
            || REBUILT_FOR_CHILD.contains(&name)
            || NEVER_INHERITED.contains(&name)
            || registry.get(name).is_some()
        {
            continue;
        }
        registry.register_shared(Arc::clone(tool));
    }
    (registry, plan_source)
}

fn definition_policy(
    parent: &ToolPolicy,
    allowed: &[String],
    denied: &[String],
    parent_tools: &[Arc<dyn Tool>],
) -> ToolPolicy {
    let mut candidates = parent_tools
        .iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    candidates.extend(REBUILT_FOR_CHILD.iter().map(|name| (*name).to_string()));
    candidates.push("Spawn".to_string());
    candidates.push("ToolSearch".to_string());
    ToolPolicy::allow_only(
        candidates.into_iter().filter(|name| {
            parent.allows(name) && (allowed.is_empty() || allowed.contains(name)) && !denied.contains(name)
        }),
    )
}

fn build_task_tracking(
    registry: &mut ToolRegistry,
    policy: &ToolPolicy,
    config: &Config,
    cwd: &Path,
) -> Option<PlanSource> {
    if !config.todo.enabled {
        return None;
    }
    match config.todo.mode {
        TodoMode::List if policy.allows("TodoWrite") => {
            let store = Arc::new(TodoStore::new());
            registry.register(Box::new(TodoWriteTool::new(
                Arc::clone(&store),
                config.todo.allow_parallel_in_progress,
            )));
            Some(PlanSource::List(store))
        }
        TodoMode::Graph => {
            let store = Arc::new(TaskStore::new(task_dir(cwd)));
            let tools: Vec<(&str, Box<dyn agentrs_tools::Tool>)> = vec![
                ("TaskCreate", Box::new(TaskCreateTool::new(Arc::clone(&store)))),
                ("TaskList", Box::new(TaskListTool::new(Arc::clone(&store)))),
                ("TaskGet", Box::new(TaskGetTool::new(Arc::clone(&store)))),
                ("TaskUpdate", Box::new(TaskUpdateTool::new(Arc::clone(&store)))),
            ];
            let mut registered = false;
            for (name, tool) in tools {
                if policy.allows(name) {
                    registry.register(tool);
                    registered = true;
                }
            }
            registered.then_some(PlanSource::Graph(store))
        }
        TodoMode::List => None,
    }
}

#[cfg(test)]
#[path = "../spawner_test.rs"]
mod spawner_test;
