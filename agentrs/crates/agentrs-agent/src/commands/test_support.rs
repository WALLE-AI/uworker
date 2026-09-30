//! Shared scaffolding for slash-command tests: a sink that records what the
//! user was told, and an owner for everything `CommandContext` borrows.

use std::sync::{Arc, Mutex};

use agentrs_providers::{LlmProvider, ProviderError};
use agentrs_types::llm::{LlmEvent, LlmRequest};
use agentrs_types::message::Message;
use agentrs_types::tool::ToolDef;

use super::{CommandContext, CommandRegistry};
use crate::compact::state::CompactState;
use crate::context_usage::{ContextState, PromptUsage};
use crate::output::OutputSink;
use crate::subagent::registry::SubAgentRegistry;
use crate::team::InProcessTeamRuntime;

pub(super) struct NullProvider;

#[async_trait::async_trait]
impl LlmProvider for NullProvider {
    async fn stream(&self, _: &LlmRequest) -> Result<tokio::sync::mpsc::Receiver<LlmEvent>, ProviderError> {
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        Ok(rx)
    }
}

/// Captures what a command told the user, so assertions can read it back.
#[derive(Default)]
pub(super) struct RecordingSink {
    info: Mutex<Vec<String>>,
    errors: Mutex<Vec<String>>,
}

impl RecordingSink {
    pub(super) fn info(&self) -> String {
        self.info
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .join("\n")
    }

    pub(super) fn errors(&self) -> String {
        self.errors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .join("\n")
    }
}

impl OutputSink for RecordingSink {
    fn emit_text_delta(&self, _: &str, _: &str) {}
    fn emit_thinking(&self, _: &str, _: &str) {}
    fn emit_tool_call(&self, _: &str, _: &str, _: &str) {}
    fn emit_tool_result(&self, _: &str, _: &str, _: bool, _: &str) {}
    fn emit_stream_start(&self, _: &str) {}
    fn emit_stream_end(&self, _: &str, _: usize, _: u64, _: u64, _: u64, _: u64) {}
    fn emit_error(&self, text: &str) {
        self.errors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(text.to_owned());
    }
    fn emit_info(&self, text: &str) {
        self.info
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(text.to_owned());
    }
}

/// Owns everything `CommandContext` borrows so a test can build one in a line.
pub(super) struct Harness {
    messages: Vec<Message>,
    compact_state: CompactState,
    compact_config: agentrs_config::compact::CompactConfig,
    context_state: ContextState,
    prompt_usage: PromptUsage,
    context_tools: Vec<ToolDef>,
    registry: CommandRegistry,
    pub(super) output: RecordingSink,
    subagent_registry: Option<Arc<SubAgentRegistry>>,
    team_runtime: Option<Arc<InProcessTeamRuntime>>,
}

impl Harness {
    pub(super) fn new() -> Self {
        Self {
            messages: Vec::new(),
            compact_state: CompactState::new(),
            compact_config: Default::default(),
            context_state: ContextState::default(),
            prompt_usage: PromptUsage::default(),
            context_tools: Vec::new(),
            registry: CommandRegistry::new(),
            output: RecordingSink::default(),
            subagent_registry: None,
            team_runtime: None,
        }
    }

    pub(super) fn with_subagents(mut self, registry: Arc<SubAgentRegistry>) -> Self {
        self.subagent_registry = Some(registry);
        self
    }

    pub(super) fn with_team(mut self, runtime: Arc<InProcessTeamRuntime>) -> Self {
        self.team_runtime = Some(runtime);
        self
    }

    pub(super) fn context(&mut self) -> CommandContext<'_> {
        CommandContext {
            messages: &mut self.messages,
            compact_state: &mut self.compact_state,
            compact_config: &self.compact_config,
            provider: Arc::new(NullProvider),
            model: "test",
            internal_summarizer: None,
            output: &self.output,
            registry: &self.registry,
            context_state: &mut self.context_state,
            prompt_usage: &self.prompt_usage,
            context_tools: &self.context_tools,
            memory_dir: None,
            subagent_registry: self.subagent_registry.as_ref(),
            team_runtime: self.team_runtime.as_ref(),
            dynamic_system_tokens: 0,
        }
    }
}
