use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use agentrs_config::config::{Config, McpServerConfig};
use agentrs_config::shell::{ResolvedShell, resolve_shell_config};
use agentrs_config::todo::TodoMode;
use agentrs_mcp::manager::McpManager;
use agentrs_mcp::tool_proxy::register_mcp_tools;
use agentrs_memory::paths::{ENTRYPOINT_NAME, auto_memory_dir};
use agentrs_providers::{LlmProvider, create_provider};
use agentrs_skills::loader::load_all_skills;
use agentrs_skills::permissions::SkillPermissionChecker;
use agentrs_skills::types::SkillMetadata;
use agentrs_tools::context::ToolContext;
use agentrs_tools::edit::EditTool;
use agentrs_tools::exec_command::ExecCommandTool;
use agentrs_tools::file_cache::FileStateCache;
use agentrs_tools::glob::GlobTool;
use agentrs_tools::grep::GrepTool;
use agentrs_tools::read::ReadTool;
use agentrs_tools::registry::ToolRegistry;
use agentrs_tools::task::{TaskCreateTool, TaskGetTool, TaskListTool, TaskStore, TaskUpdateTool, task_dir};
use agentrs_tools::todo::{TodoStore, TodoWriteTool};
use agentrs_tools::tool_search::ToolSearchTool;
use agentrs_tools::view_image::ViewImageTool;
use agentrs_tools::web::fetch_tool::WebFetchTool;
use agentrs_tools::web::search_tool::WebSearchTool;
use agentrs_tools::web::{BackendError, build_backend};
use agentrs_tools::write::WriteTool;
use anyhow::Result;
use tracing::info;

use crate::context::{SystemPromptCache, build_system_prompt_with_shell_and_tool_policy};
use crate::context_usage::PromptUsage;
use crate::engine::AgentEngine;
use crate::output::OutputSink;
use crate::plan::tools::{EnterPlanModeTool, ExitPlanModeTool};
use crate::session::Session;
use crate::skill_tool::SkillTool;
use crate::spawn_tool::SpawnTool;
use crate::spawner::AgentSpawner;
use crate::subagent::definitions::AgentDefinitions;
use crate::summarizer::ProviderSummarizer;
use crate::todo_reminder::{PlanSource, TodoRuntime};
use crate::tool_policy::ToolPolicy;

/// Result of bootstrapping an agent engine with all features initialized.
pub struct BootstrapResult {
    // Fully initialized runtime.
    pub engine: AgentEngine,

    // Shared provider dependency created or reused during bootstrap.
    pub provider: Arc<dyn LlmProvider>,

    // MCP runtime state discovered during bootstrap.
    pub mcp_managers: Vec<Arc<McpManager>>,
    pub has_mcp: bool,
}

/// Builder for creating a fully-initialized `AgentEngine`.
///
/// Encapsulates the complete initialization pipeline so all consumers
/// (CLI, backend, sub-agents) get consistent behavior:
///
/// - System prompt always includes model identity, working directory, date
/// - Tool usage guidance is always injected
/// - AGENTS.md is loaded from the workspace hierarchy
/// - Skills, MCP, plan mode, spawn are enabled based on `Config` fields
pub struct AgentBootstrap {
    // Bootstrap configuration.
    config: Config,
    workspace: PathBuf,
    extra_skill_dirs: Vec<PathBuf>,
    isolate_skill_dirs: bool,

    // Output integration.
    output: Arc<dyn OutputSink>,

    // Optional externally supplied runtime state.
    provider: Option<Arc<dyn LlmProvider>>,
    resume_session: Option<Session>,
    runtime_env: Vec<(String, String)>,
    tool_policy: ToolPolicy,
}

struct BootstrapEnvironment {
    // Workspace context.
    workspace: PathBuf,

    // Prompt context.
    resolved_shell: ResolvedShell,
    memory_dir: Option<PathBuf>,
}

#[derive(Default)]
struct McpBootstrap {
    // Active manager used for MCP-backed skills.
    manager: Option<Arc<McpManager>>,

    // Managers retained by the caller for lifecycle ownership.
    managers: Vec<Arc<McpManager>>,
}

impl McpBootstrap {
    fn has_mcp(&self) -> bool {
        self.manager.is_some()
    }
}

impl AgentBootstrap {
    pub fn new(config: Config, workspace: impl Into<String>, output: Arc<dyn OutputSink>) -> Self {
        Self {
            config,
            workspace: PathBuf::from(workspace.into()),
            extra_skill_dirs: Vec::new(),
            isolate_skill_dirs: false,
            output,
            provider: None,
            resume_session: None,
            runtime_env: Vec::new(),
            tool_policy: ToolPolicy::default(),
        }
    }

    /// Use a pre-created provider instead of creating one from config.
    pub fn provider(mut self, provider: Arc<dyn LlmProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Resume from a previously saved session.
    pub fn resume(mut self, session: Session) -> Self {
        self.resume_session = Some(session);
        self
    }

    /// Inject process environment for tools/hooks/MCP subprocesses owned by this engine.
    pub fn runtime_env(mut self, runtime_env: Vec<(String, String)>) -> Self {
        self.runtime_env = runtime_env;
        self
    }

    /// Restrict which registered tools can be advertised and executed.
    pub fn tool_policy(mut self, tool_policy: ToolPolicy) -> Self {
        self.tool_policy = tool_policy;
        self
    }

    /// Add extra directories to scan for skills.
    pub fn extra_skill_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.extra_skill_dirs = dirs;
        self
    }

    /// Discover skills only from the supplied directories and bundled skills.
    ///
    /// Each directory is interpreted as a project root containing
    /// `.agentrs/skills`.
    pub fn isolated_skill_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.extra_skill_dirs = dirs;
        self.isolate_skill_dirs = true;
        self
    }

    /// Read-only access to the config (for session management before build).
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Build the fully-initialized engine.
    pub async fn build(mut self) -> Result<BootstrapResult> {
        let workspace = self.resolve_workspace_path();
        let provider = self.resolve_provider();
        let environment = self.resolve_environment(workspace)?;
        let mut registry = self.build_builtin_registry(&environment.workspace);

        let builtin_names = registry.tool_names();
        let mcp = self.connect_mcp(&mut registry, &builtin_names).await;

        let skills = self.load_skills(&environment.workspace, mcp.manager.as_deref()).await;
        let prompt_usage = self.configure_system_prompt(&environment, &skills);

        let subagent_registry = self.register_agent_tools(&mut registry, &provider, &environment.workspace, skills);
        let plan_active_flag = self.register_plan_tools(&mut registry);
        let plan_source = self.register_task_tracking(&mut registry, &environment.workspace);
        self.register_tool_search(&mut registry);

        let has_mcp = mcp.has_mcp();
        let mcp_managers = mcp.managers;
        let mut engine = self.into_engine(
            provider.clone(),
            registry,
            plan_active_flag,
            plan_source,
            environment.workspace,
            prompt_usage,
        );
        engine.set_subagent_runtime(subagent_registry.0, subagent_registry.1);

        Ok(BootstrapResult {
            engine,
            provider,
            mcp_managers,
            has_mcp,
        })
    }

    fn resolve_workspace_path(&self) -> PathBuf {
        info!(
            target: "agentrs_agent",
            workspace = %self.workspace.display(),
            "agent bootstrap: workspace cwd resolved",
        );

        self.workspace.clone()
    }

    fn resolve_environment(&self, workspace_path: PathBuf) -> Result<BootstrapEnvironment> {
        Ok(BootstrapEnvironment {
            resolved_shell: resolve_shell_config(&self.config.shell)?,
            memory_dir: auto_memory_dir(&workspace_path),
            workspace: workspace_path,
        })
    }

    fn resolve_provider(&mut self) -> Arc<dyn LlmProvider> {
        self.provider.take().unwrap_or_else(|| create_provider(&self.config))
    }

    fn build_builtin_registry(&self, workspace_path: &Path) -> ToolRegistry {
        let file_cache = self.build_file_cache();
        let tool_context = ToolContext::new(file_cache);
        let mut registry = ToolRegistry::new();

        registry.register(Box::new(ReadTool::with_context(tool_context.clone())));
        registry.register(Box::new(WriteTool::with_context(tool_context.clone())));
        registry.register(Box::new(EditTool::with_context(tool_context)));
        registry.register(Box::new(ExecCommandTool::new_with_env(
            workspace_path.to_path_buf(),
            self.runtime_env.clone(),
        )));
        registry.register(Box::new(GrepTool::new(workspace_path.to_path_buf())));
        registry.register(Box::new(GlobTool::new(workspace_path.to_path_buf())));
        registry.register(Box::new(ViewImageTool::new()));

        registry
    }

    fn build_file_cache(&self) -> Option<Arc<RwLock<FileStateCache>>> {
        self.config
            .file_cache
            .enabled
            .then(|| Arc::new(RwLock::new(FileStateCache::new(&self.config.file_cache))))
    }

    async fn connect_mcp(&self, registry: &mut ToolRegistry, builtin_names: &[String]) -> McpBootstrap {
        let server_configs = self.mcp_servers_with_runtime_env();
        if server_configs.is_empty() {
            return McpBootstrap::default();
        }

        let manager = match McpManager::connect_all(&server_configs).await {
            Ok(manager) => Arc::new(manager),
            Err(err) => {
                self.output.emit_error(&format!("MCP initialization error: {err}"));
                return McpBootstrap::default();
            }
        };

        register_mcp_tools(registry, &manager, builtin_names, &server_configs);

        McpBootstrap {
            manager: Some(Arc::clone(&manager)),
            managers: vec![manager],
        }
    }

    fn mcp_servers_with_runtime_env(&self) -> HashMap<String, McpServerConfig> {
        let mut servers = self.config.mcp.servers.clone();
        if self.runtime_env.is_empty() {
            return servers;
        }

        for server in servers.values_mut() {
            let mut env: HashMap<String, String> = self.runtime_env.clone().into_iter().collect();
            if let Some(server_env) = server.env.take() {
                env.extend(server_env);
            }
            server.env = Some(env);
        }

        servers
    }

    async fn load_skills(&self, workspace: &Path, mcp_manager: Option<&McpManager>) -> Vec<SkillMetadata> {
        load_all_skills(workspace, &self.extra_skill_dirs, self.isolate_skill_dirs, mcp_manager).await
    }

    fn configure_system_prompt(&mut self, environment: &BootstrapEnvironment, skills: &[SkillMetadata]) -> PromptUsage {
        let mut prompt_cache = SystemPromptCache::new();
        let workspace = self.workspace.to_string_lossy();
        let mut system_prompt = build_system_prompt_with_shell_and_tool_policy(
            &mut prompt_cache,
            self.config.system_prompt.as_deref(),
            &workspace,
            &self.config.model,
            &environment.resolved_shell,
            skills,
            None,
            environment.memory_dir.as_deref(),
            false,
            self.config.compact.toon,
            &self.tool_policy,
        );
        if self.config.subagent.enabled && self.tool_policy.allows("Spawn") {
            let subagents =
                AgentDefinitions::load(&environment.workspace, self.config.subagent.builtin_agents).prompt_section();
            if !subagents.is_empty() {
                system_prompt.push_str("\n\n");
                system_prompt.push_str(&subagents);
                prompt_cache.sections.insert("subagents", subagents);
            }
        }
        let memory_prompt = prompt_cache.sections.get("memory").map(String::as_str);
        let skills_prompt = prompt_cache.sections.get("skills").map(String::as_str);
        let memory_files = if memory_prompt.is_some() {
            environment
                .memory_dir
                .as_ref()
                .map(|directory| directory.join(ENTRYPOINT_NAME))
                .filter(|path| path.is_file())
                .map(|path| path.display().to_string())
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        let visible_skills = skills
            .iter()
            .filter(|skill| self.tool_policy.allows("Skill") && !skill.disable_model_invocation)
            .map(|skill| skill.name.clone())
            .collect();
        let prompt_usage = PromptUsage::from_sections(
            &system_prompt,
            memory_prompt,
            skills_prompt,
            memory_files,
            visible_skills,
        );
        self.config.system_prompt = Some(system_prompt);
        prompt_usage
    }

    fn register_agent_tools(
        &self,
        registry: &mut ToolRegistry,
        provider: &Arc<dyn LlmProvider>,
        workspace: &Path,
        skills: Vec<SkillMetadata>,
    ) -> (
        Arc<crate::subagent::registry::SubAgentRegistry>,
        Arc<RwLock<Option<String>>>,
    ) {
        self.register_web_tools(registry, provider, workspace);
        let parent_tools = registry.shared_tools();
        let spawner = Arc::new(
            AgentSpawner::new_with_env(
                Arc::clone(provider),
                self.config.clone(),
                workspace.to_path_buf(),
                self.runtime_env.clone(),
                self.tool_policy.clone(),
            )
            .with_progress_output(Arc::clone(&self.output))
            .with_parent_tools(parent_tools),
        );
        let skill_checker = SkillPermissionChecker::new(
            self.config.tools.skills.deny.clone(),
            self.config.tools.skills.allow.clone(),
            self.config.tools.auto_approve,
        );
        registry.register(Box::new(SkillTool::with_spawner(
            Arc::new(skills),
            self.workspace.to_path_buf(),
            skill_checker,
            self.resume_session.as_ref().map(|session| session.id.clone()),
            self.config
                .subagent
                .enabled
                .then(|| spawner.clone() as Arc<dyn agentrs_types::subagent::Spawner>),
        )));

        if self.config.subagent.enabled && self.tool_policy.allows("Spawn") {
            registry.register(Box::new(SpawnTool::new(Arc::clone(&spawner))));
        }
        (spawner.registry(), spawner.parent_session_slot())
    }

    /// Register the network tools, when configured.
    ///
    /// Both are skipped rather than registered-and-failing when unusable: a
    /// tool the model can see but never call wastes context on every request.
    fn register_web_tools(&self, registry: &mut ToolRegistry, provider: &Arc<dyn LlmProvider>, workspace: &Path) {
        let web = &self.config.web;
        if !web.enabled {
            info!(target: "agentrs_agent", "web tools disabled by configuration");
            return;
        }

        let definitions = AgentDefinitions::load(workspace, self.config.subagent.builtin_agents);
        let summarizer = Arc::new(
            ProviderSummarizer::new(Arc::clone(provider), self.config.model.clone())
                .with_definition(definitions.resolve(Some("summarize"))),
        );
        match WebFetchTool::new(web, web_download_dir(workspace), Some(summarizer)) {
            Ok(tool) => registry.register(Box::new(tool)),
            Err(error) => tracing::warn!(
                target: "agentrs_agent",
                %error,
                "WebFetch could not be initialized and will not be available"
            ),
        }

        match build_backend(web) {
            Ok(backend) => {
                info!(target: "agentrs_agent", backend = backend.name(), "WebSearch enabled");
                registry.register(Box::new(WebSearchTool::new(backend, web.search.max_results)));
            }
            Err(BackendError::Disabled) => {
                info!(target: "agentrs_agent", "WebSearch disabled: no search backend configured");
            }
            Err(BackendError::Misconfigured(reason)) => tracing::warn!(
                target: "agentrs_agent",
                %reason,
                "WebSearch is configured but unusable and will not be available"
            ),
        }
    }

    fn register_plan_tools(&self, registry: &mut ToolRegistry) -> Arc<AtomicBool> {
        let plan_active_flag = Arc::new(AtomicBool::new(false));

        if self.config.plan.enabled {
            registry.register(Box::new(EnterPlanModeTool::new(Arc::clone(&plan_active_flag))));
            registry.register(Box::new(ExitPlanModeTool::new(Arc::clone(&plan_active_flag))));
        }

        plan_active_flag
    }

    /// Register whichever task-tracking tools the configured mode calls for,
    /// and hand back the store the engine should read the plan from.
    ///
    /// `None` when tracking is switched off entirely, which is also what tells
    /// the engine there is nothing to publish or remind about.
    fn register_task_tracking(&self, registry: &mut ToolRegistry, workspace: &Path) -> Option<PlanSource> {
        if !self.config.todo.enabled {
            return None;
        }

        match self.config.todo.mode {
            TodoMode::List => {
                let store = Arc::new(TodoStore::new());
                registry.register(Box::new(TodoWriteTool::new(
                    Arc::clone(&store),
                    self.config.todo.allow_parallel_in_progress,
                )));
                Some(PlanSource::List(store))
            }
            TodoMode::Graph => {
                let store = Arc::new(TaskStore::new(task_dir(workspace)));
                registry.register(Box::new(TaskCreateTool::new(Arc::clone(&store))));
                registry.register(Box::new(TaskListTool::new(Arc::clone(&store))));
                registry.register(Box::new(TaskGetTool::new(Arc::clone(&store))));
                registry.register(Box::new(TaskUpdateTool::new(Arc::clone(&store))));
                Some(PlanSource::Graph(store))
            }
        }
    }

    fn register_tool_search(&self, registry: &mut ToolRegistry) {
        let tool_defs_snapshot = registry.to_tool_defs_filtered(|tool| self.tool_policy.allows(tool.name()));
        registry.register(Box::new(ToolSearchTool::new(tool_defs_snapshot)));
    }

    fn into_engine(
        self,
        provider: Arc<dyn LlmProvider>,
        registry: ToolRegistry,
        plan_active_flag: Arc<AtomicBool>,
        plan_source: Option<PlanSource>,
        workspace: PathBuf,
        prompt_usage: PromptUsage,
    ) -> AgentEngine {
        let reminder_turns = self.config.todo.reminder_turns;
        let runtime_env = self.runtime_env.clone();
        let mut engine = if let Some(session) = self.resume_session {
            AgentEngine::resume_with_provider_and_env(
                provider,
                self.config,
                registry,
                self.output,
                session,
                workspace,
                runtime_env,
            )
        } else {
            AgentEngine::new_with_provider_and_env(provider, self.config, registry, self.output, workspace, runtime_env)
        };
        engine.set_plan_active_flag(plan_active_flag);
        if let Some(source) = plan_source {
            engine.set_todo_runtime(TodoRuntime::new(source, reminder_turns));
        }
        engine.set_tool_policy(self.tool_policy);
        engine.set_prompt_usage(prompt_usage);
        engine
    }
}

/// Where `WebFetch` saves binary payloads it cannot render as text.
///
/// Kept inside the workspace, next to session state, so the files are visible
/// to the user and removed with the rest of the agent's scratch data.
fn web_download_dir(workspace: &Path) -> PathBuf {
    workspace.join(".agentrs").join("webfetch")
}

#[cfg(test)]
#[path = "bootstrap_test.rs"]
mod bootstrap_test;
