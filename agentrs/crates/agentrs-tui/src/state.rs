use std::collections::{HashMap, HashSet};

use agentrs_agent::commands::CommandSpec;
use agentrs_protocol::events::TodoSnapshot;
use agentrs_types::message::{Message, TokenUsage};

use crate::app_command::application_command_specs;
use crate::command_popup::CommandPopup;
use crate::composer::Composer;
use crate::event::AgentEvent;
use crate::session_picker::SessionPicker;
use crate::transcript::{EntryKind, ToolStepStatus, TranscriptEntry, entries_from_messages};
use agentrs_config::tui::ThinkingDisplay;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApprovalChoice {
    Once,
    Always,
    Deny,
}

impl ApprovalChoice {
    pub(super) fn previous(self) -> Self {
        match self {
            Self::Once => Self::Deny,
            Self::Always => Self::Once,
            Self::Deny => Self::Always,
        }
    }

    pub(super) fn next(self) -> Self {
        match self {
            Self::Once => Self::Always,
            Self::Always => Self::Deny,
            Self::Deny => Self::Once,
        }
    }
}

#[derive(Debug)]
pub(super) struct ApprovalRequest {
    pub(super) call_id: String,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) input: String,
    pub(super) choice: ApprovalChoice,
}

#[derive(Debug)]
pub(super) struct AppState {
    pub(super) model: String,
    pub(super) provider: String,
    pub(super) cwd: String,
    pub(super) session_id: Option<String>,
    pub(super) no_color: bool,
    pub(super) thinking_display: ThinkingDisplay,
    pub(super) composer: Composer,
    pub(super) popup: CommandPopup,
    pub(super) session_picker: SessionPicker,
    pub(super) transcript: Vec<TranscriptEntry>,
    pub(super) committed_transcript: usize,
    pub(super) show_welcome: bool,
    pub(super) approval: Option<ApprovalRequest>,
    pub(super) initializing: bool,
    pub(super) busy: bool,
    pub(super) spinner_frame: usize,
    pub(super) usage: TokenUsage,
    pub(super) turns: usize,
    /// Latest task checklist published by the agent. Whole-list replacement,
    /// mirroring the tool's own semantics.
    pub(super) todos: Vec<TodoSnapshot>,
    active_assistant: Option<usize>,
    active_thinking: Option<usize>,
    active_tools: HashMap<String, usize>,
    protocol_results: HashSet<String>,
}

impl AppState {
    /// Test-only shorthand; production always passes an explicit display mode.
    #[cfg(test)]
    pub(super) fn new(model: String, provider: String, cwd: String, no_color: bool) -> Self {
        Self::with_thinking_display(model, provider, cwd, no_color, ThinkingDisplay::default())
    }

    pub(super) fn with_thinking_display(
        model: String,
        provider: String,
        cwd: String,
        no_color: bool,
        thinking_display: ThinkingDisplay,
    ) -> Self {
        Self {
            model,
            provider,
            cwd,
            session_id: None,
            no_color,
            thinking_display,
            composer: Composer::default(),
            popup: CommandPopup::default(),
            session_picker: SessionPicker::default(),
            transcript: Vec::new(),
            committed_transcript: 0,
            show_welcome: true,
            approval: None,
            initializing: false,
            busy: false,
            spinner_frame: 0,
            usage: TokenUsage::default(),
            turns: 0,
            todos: Vec::new(),
            active_assistant: None,
            active_thinking: None,
            active_tools: HashMap::new(),
            protocol_results: HashSet::new(),
        }
    }

    pub(super) fn set_commands(&mut self, commands: Vec<CommandSpec>) {
        let mut commands = commands;
        commands.extend(application_command_specs());
        commands.sort_by(|left, right| left.name.cmp(&right.name));
        commands.dedup_by(|left, right| left.name == right.name);
        self.popup.set_commands(commands);
    }

    pub(super) fn set_history(&mut self, messages: &[Message]) {
        self.transcript = entries_from_messages(messages);
        self.committed_transcript = 0;
        self.show_welcome = messages.is_empty();
        self.close_active_blocks();
        self.active_tools.clear();
    }

    pub(super) fn begin_initialization(&mut self) {
        self.initializing = true;
    }

    pub(super) fn reset_session(
        &mut self,
        model: String,
        provider: String,
        session_id: Option<String>,
        messages: &[Message],
    ) {
        self.model = model;
        self.provider = provider;
        self.session_id = session_id;
        self.set_history(messages);
        self.approval = None;
        self.initializing = false;
        self.busy = false;
        self.spinner_frame = 0;
        self.usage = TokenUsage::default();
        self.turns = 0;
        self.todos.clear();
        self.composer.clear();
        self.popup.update("");
        self.session_picker.close();
        self.active_tools.clear();
        self.protocol_results.clear();
    }

    pub(super) fn pending_transcript(&self) -> &[TranscriptEntry] {
        &self.transcript[self.committed_transcript.min(self.transcript.len())..]
    }

    pub(super) fn mark_transcript_committed(&mut self) {
        self.committed_transcript = self.transcript.len();
    }

    pub(super) fn prepare_history_replay(&mut self) -> usize {
        self.committed_transcript = 0;
        for entry in &mut self.transcript {
            entry.reset_display_offset();
        }
        self.show_welcome = self.transcript.is_empty();

        let first_unstable_tool = self.transcript.iter().position(|entry| !entry.is_stable_for_history());
        [self.active_assistant, self.active_thinking, first_unstable_tool]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(self.transcript.len())
    }

    pub(super) fn mark_transcript_prefix_committed(&mut self, count: usize) {
        self.committed_transcript = count.min(self.transcript.len());
    }

    pub(super) fn commit_streaming_prefix(&mut self, complete_entries: usize, active_byte_count: usize) {
        let pending_count = self.pending_transcript().len();
        self.committed_transcript = self
            .committed_transcript
            .saturating_add(complete_entries.min(pending_count));
        if active_byte_count > 0
            && let Some(active) = self.transcript.get_mut(self.committed_transcript)
        {
            active.advance_display_offset(active_byte_count);
        }
    }

    pub(super) fn can_commit_streaming_lines(&self) -> bool {
        self.busy
            && self
                .pending_transcript()
                .last()
                .is_some_and(|entry| matches!(entry.kind, EntryKind::Assistant | EntryKind::Thinking))
    }

    pub(super) fn push_info(&mut self, label: &str, text: impl Into<String>) {
        self.transcript.push(TranscriptEntry::new(EntryKind::Info, label, text));
    }

    pub(super) fn push_error(&mut self, text: impl Into<String>) {
        self.transcript
            .push(TranscriptEntry::new(EntryKind::Error, "Error", text));
    }

    pub(super) fn begin_turn(&mut self, input: &str) {
        self.busy = true;
        self.close_active_blocks();
        if !self.popup.recognizes(input) {
            self.transcript.push(TranscriptEntry::new(EntryKind::User, "", input));
        }
    }

    pub(super) fn finish_turn(&mut self, turns: usize, usage: TokenUsage) {
        self.busy = false;
        self.turns = turns;
        self.usage = usage;
        self.close_active_blocks();
    }

    pub(super) fn cancel_turn(&mut self) {
        self.busy = false;
        self.close_active_blocks();
        self.transcript.push(TranscriptEntry::new(
            EntryKind::Info,
            "Stopped",
            "Turn cancelled by user",
        ));
    }

    pub(super) fn tick(&mut self) {
        if self.busy {
            self.spinner_frame = (self.spinner_frame + 1) % 4;
        }
    }

    pub(super) fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::StreamStart => self.close_active_blocks(),
            AgentEvent::TextDelta(text) => self.append_stream(EntryKind::Assistant, "", text),
            AgentEvent::Thinking(text) => {
                if self.thinking_display.is_visible() {
                    self.append_stream(EntryKind::Thinking, "Thinking", text);
                }
            }
            AgentEvent::Info(text) => self
                .transcript
                .push(TranscriptEntry::new(EntryKind::Info, "Info", text)),
            AgentEvent::Error(text) => self
                .transcript
                .push(TranscriptEntry::new(EntryKind::Error, "Error", text)),
            AgentEvent::ToolCall { call_id, name, input } => {
                // A tool call closes the open text block. Without this the
                // model's post-tool answer is appended to the entry created
                // before the call, so the transcript shows the answer above
                // the tools that produced it.
                self.close_active_blocks();
                self.update_tool_step(&call_id, &name, ToolStepStatus::Queued, Some(input))
            }
            AgentEvent::ToolResult {
                call_id,
                name,
                is_error,
                content,
            } => {
                if self.protocol_results.remove(&call_id) {
                    return;
                }
                let status = if is_error {
                    ToolStepStatus::Error
                } else {
                    ToolStepStatus::Success
                };
                self.update_tool_step(&call_id, &name, status, Some(content));
            }
            AgentEvent::ProtocolToolResult {
                call_id,
                name,
                is_error,
                content,
            } => {
                let status = if is_error {
                    ToolStepStatus::Error
                } else {
                    ToolStepStatus::Success
                };
                self.update_tool_step(&call_id, &name, status, Some(content));
                self.protocol_results.insert(call_id);
            }
            AgentEvent::ApprovalRequested {
                call_id,
                name,
                description,
                input,
            } => {
                self.update_tool_step(&call_id, &name, ToolStepStatus::Approval, None);
                self.approval = Some(ApprovalRequest {
                    call_id,
                    name,
                    description,
                    input,
                    choice: ApprovalChoice::Once,
                });
            }
            AgentEvent::ToolRunning { call_id, name } => {
                if self.approval.as_ref().is_some_and(|request| request.call_id == call_id) {
                    self.approval = None;
                }
                self.update_tool_step(&call_id, &name, ToolStepStatus::Running, None);
            }
            AgentEvent::ToolCancelled { call_id, name, reason } => {
                if self.approval.as_ref().is_some_and(|request| request.call_id == call_id) {
                    self.approval = None;
                }
                self.update_tool_step(&call_id, &name, ToolStepStatus::Cancelled, Some(reason));
            }
            AgentEvent::TodoUpdated(todos) => self.todos = todos,
            AgentEvent::SubAgentStarted { id, name, depth } => {
                self.close_active_blocks();
                self.transcript.push(TranscriptEntry::new(
                    EntryKind::Info,
                    "Sub-agent",
                    format!(
                        "{name} started ({}..., depth {depth})",
                        id.chars().take(8).collect::<String>()
                    ),
                ));
            }
            AgentEvent::SubAgentProgress {
                id,
                status,
                turns,
                output_tokens,
            } => self.transcript.push(TranscriptEntry::new(
                EntryKind::Info,
                "Sub-agent",
                format!(
                    "{}...: {status:?}, {turns} turns, {output_tokens} output tokens",
                    id.chars().take(8).collect::<String>()
                ),
            )),
            AgentEvent::SubAgentFinished {
                id,
                status,
                turns,
                output_tokens,
            } => self.transcript.push(TranscriptEntry::new(
                EntryKind::Info,
                "Sub-agent",
                format!(
                    "{}... finished: {status:?}, {turns} turns, {output_tokens} output tokens",
                    id.chars().take(8).collect::<String>()
                ),
            )),
        }
    }

    /// The entry the agent is working on right now, if exactly the one.
    ///
    /// `None` once several are in progress: naming one of them in the status
    /// line would be arbitrary, and naming all of them would not fit.
    pub(super) fn active_todo(&self) -> Option<&TodoSnapshot> {
        let mut active = self.todos.iter().filter(|todo| todo.status == "in_progress");
        let first = active.next()?;
        active.next().is_none().then_some(first)
    }

    /// The full checklist as text, for `/todos`.
    ///
    /// Unlike the panel this elides nothing: the command exists precisely to
    /// see the entries the panel had to cut.
    pub(super) fn todo_summary(&self) -> String {
        if self.todos.is_empty() {
            return "No tasks tracked in this session".to_string();
        }
        let done = self.todos.iter().filter(|todo| todo.status == "completed").count();
        let mut text = format!("{}/{} completed", done, self.todos.len());
        for todo in &self.todos {
            let marker = match todo.status.as_str() {
                "completed" => "✓",
                "in_progress" => "▸",
                _ => "○",
            };
            text.push_str(&format!("\n  {marker} {}", todo.content));
        }
        text
    }

    /// Text for the busy status line: the active task's present-continuous
    /// form, falling back to its content when the model omitted `activeForm`.
    pub(super) fn active_todo_label(&self) -> Option<&str> {
        self.active_todo()
            .map(|todo| todo.active_form.as_deref().unwrap_or(&todo.content))
    }

    fn update_tool_step(&mut self, call_id: &str, name: &str, status: ToolStepStatus, text: Option<String>) {
        if let Some(index) = self.active_tools.get(call_id).copied()
            && let Some(entry) = self.transcript.get_mut(index)
        {
            if name != "tool" {
                entry.label = name.to_string();
            }
            entry.tool_status = Some(status);
            if let Some(text) = text {
                entry.text = text;
            }
            return;
        }

        let index = self.transcript.len();
        self.transcript
            .push(TranscriptEntry::tool(name, text.unwrap_or_default(), status));
        self.active_tools.insert(call_id.to_string(), index);
    }

    fn append_stream(&mut self, kind: EntryKind, label: &str, text: String) {
        // Switching kind ends the block in flight. Reasoning usually runs
        // straight into the answer with no tool call between them, so this is
        // the boundary that lets a reasoning block finish — and therefore
        // collapse — in the common case.
        match kind {
            EntryKind::Assistant => self.close_active(BlockSlot::Thinking),
            EntryKind::Thinking => self.close_active(BlockSlot::Assistant),
            _ => return,
        }

        let active = match kind {
            EntryKind::Assistant => &mut self.active_assistant,
            EntryKind::Thinking => &mut self.active_thinking,
            _ => return,
        };
        if let Some(index) = *active {
            self.transcript[index].text.push_str(&text);
        } else {
            let mut entry = TranscriptEntry::new(kind, label, text);
            entry.start_clock();
            self.transcript.push(entry);
            *active = Some(self.transcript.len() - 1);
        }
    }

    /// Close whatever text blocks are still streaming.
    ///
    /// Every path that ends a block funnels through here so a finished block
    /// always carries its duration — that is what a collapsed reasoning
    /// summary reports, and what tells the renderer it may collapse at all.
    fn close_active_blocks(&mut self) {
        self.close_active(BlockSlot::Assistant);
        self.close_active(BlockSlot::Thinking);
    }

    fn close_active(&mut self, slot: BlockSlot) {
        let active = match slot {
            BlockSlot::Assistant => &mut self.active_assistant,
            BlockSlot::Thinking => &mut self.active_thinking,
        };
        if let Some(index) = active.take()
            && let Some(entry) = self.transcript.get_mut(index)
        {
            entry.finish();
        }
    }
}

/// The two streaming text blocks a turn can have open at once.
#[derive(Debug, Clone, Copy)]
enum BlockSlot {
    Assistant,
    Thinking,
}

#[cfg(test)]
#[path = "state_test.rs"]
mod state_test;
