use std::collections::{HashMap, HashSet};

use agentrs_agent::commands::CommandSpec;
use agentrs_protocol::events::{SubAgentEventStatus, TodoSnapshot};
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

/// Where an agent row came from, which decides what its line can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentOrigin {
    /// Spawned for one task and gone when it returns.
    OneShot,
    /// Persistent team member, so it also carries message counters.
    TeamMember,
}

/// One sub-agent or team member as the renderer sees it.
///
/// Lifecycle events update the row in place instead of appending a line each:
/// a handful of concurrent agents would otherwise bury the conversation they
/// were spawned from.
#[derive(Debug)]
pub(super) struct AgentRow {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) origin: AgentOrigin,
    pub(super) status: SubAgentEventStatus,
    pub(super) turns: usize,
    pub(super) output_tokens: u64,
    pub(super) messages_sent: usize,
    pub(super) messages_received: usize,
    /// Index of this row's transcript entry.
    entry: usize,
}

impl AgentRow {
    pub(super) fn is_active(&self) -> bool {
        !matches!(
            self.status,
            SubAgentEventStatus::Finished | SubAgentEventStatus::Failed | SubAgentEventStatus::Cancelled
        )
    }

    pub(super) fn status_label(&self) -> &'static str {
        match self.status {
            SubAgentEventStatus::Pending => "pending",
            SubAgentEventStatus::Running => "running",
            SubAgentEventStatus::Idle => "idle",
            SubAgentEventStatus::Finished => "done",
            SubAgentEventStatus::Failed => "failed",
            SubAgentEventStatus::Cancelled => "cancelled",
        }
    }

    /// Status as the renderer's shared step vocabulary, which is what colours
    /// the row. `Idle` reads as queued: a parked teammate is waiting, not working.
    pub(super) fn step_status(&self) -> ToolStepStatus {
        match self.status {
            SubAgentEventStatus::Pending | SubAgentEventStatus::Idle => ToolStepStatus::Queued,
            SubAgentEventStatus::Running => ToolStepStatus::Running,
            SubAgentEventStatus::Finished => ToolStepStatus::Success,
            SubAgentEventStatus::Failed => ToolStepStatus::Error,
            SubAgentEventStatus::Cancelled => ToolStepStatus::Cancelled,
        }
    }

    /// The row's detail line: work done, plus message traffic for a teammate.
    pub(super) fn detail(&self) -> String {
        let mut detail = format!(
            "{} · {} turns · {} out",
            self.status_label(),
            self.turns,
            self.output_tokens
        );
        if self.origin == AgentOrigin::TeamMember {
            detail.push_str(&format!(" · ↑{} ↓{}", self.messages_sent, self.messages_received));
        }
        detail
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
    /// Sub-agents and team members seen this session, in the order they
    /// appeared. Kept after they finish so the panel and the transcript agree.
    pub(super) agents: Vec<AgentRow>,
    active_assistant: Option<usize>,
    active_thinking: Option<usize>,
    active_tools: HashMap<String, usize>,
    /// Agent id (or team `name@team`) to its index in `agents`.
    agent_index: HashMap<String, usize>,
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
            agents: Vec::new(),
            active_assistant: None,
            active_thinking: None,
            active_tools: HashMap::new(),
            agent_index: HashMap::new(),
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
        self.clear_agents();
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
        self.clear_agents();
        self.protocol_results.clear();
    }

    fn clear_agents(&mut self) {
        self.agents.clear();
        self.agent_index.clear();
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
                let label = format!("{name} (depth {depth})");
                self.open_agent_row(id, label, AgentOrigin::OneShot, SubAgentEventStatus::Running);
            }
            AgentEvent::SubAgentProgress {
                id,
                status,
                turns,
                output_tokens,
            } => self.update_agent_row(&id, status, turns, output_tokens),
            AgentEvent::SubAgentFinished {
                id,
                status,
                turns,
                output_tokens,
            } => {
                self.update_agent_row(&id, status, turns, output_tokens);
                self.finish_agent_row(&id);
            }
            AgentEvent::TeamMemberJoined { team, member, agent_id } => {
                self.close_active_blocks();
                let label = format!("{member} ({team})");
                // A teammate starts parked on its inbox rather than working,
                // which is what `idle` says and `running` would not.
                self.open_agent_row(agent_id, label, AgentOrigin::TeamMember, SubAgentEventStatus::Idle);
            }
            AgentEvent::TeamMemberExited { agent_id } => {
                if let Some(index) = self.agent_row_index(&agent_id) {
                    self.agents[index].status = SubAgentEventStatus::Finished;
                    self.refresh_agent_entry(index);
                }
                self.finish_agent_row(&agent_id);
            }
            AgentEvent::TeamMessageSent { from, to } => self.record_team_message(&from, &to),
        }
    }

    /// Agents that have not reached a terminal status yet.
    pub(super) fn active_agents(&self) -> impl Iterator<Item = &AgentRow> {
        self.agents.iter().filter(|row| row.is_active())
    }

    fn open_agent_row(&mut self, id: String, label: String, origin: AgentOrigin, status: SubAgentEventStatus) {
        // A resumed id re-uses its row: continuing an agent is the same agent,
        // and a second row would double-count it in the panel.
        if let Some(index) = self.agent_index.get(&id).copied() {
            self.agents[index].status = status;
            self.refresh_agent_entry(index);
            return;
        }

        let entry = self.transcript.len();
        let row = AgentRow {
            id: id.clone(),
            name: label.clone(),
            origin,
            status,
            turns: 0,
            output_tokens: 0,
            messages_sent: 0,
            messages_received: 0,
            entry,
        };
        let mut transcript_entry = TranscriptEntry::agent(label, row.detail());
        transcript_entry.tool_status = Some(row.step_status());
        self.transcript.push(transcript_entry);
        self.agent_index.insert(id, self.agents.len());
        self.agents.push(row);
    }

    fn update_agent_row(&mut self, id: &str, status: SubAgentEventStatus, turns: usize, output_tokens: u64) {
        let Some(index) = self.agent_index.get(id).copied() else {
            return;
        };
        let row = &mut self.agents[index];
        row.status = status;
        row.turns = turns;
        row.output_tokens = output_tokens;
        self.refresh_agent_entry(index);
    }

    fn finish_agent_row(&mut self, id: &str) {
        let Some(index) = self.agent_index.get(id).copied() else {
            return;
        };
        let entry = self.agents[index].entry;
        if let Some(entry) = self.transcript.get_mut(entry) {
            entry.finish();
        }
    }

    /// Count one delivery against the sender and the recipients.
    ///
    /// Message bodies never reach the renderer, and a line per delivery would
    /// scale with team size squared, so traffic is reported as counters on the
    /// rows involved.
    fn record_team_message(&mut self, from: &str, to: &str) {
        let sender = self.team_row_index(from);
        if let Some(index) = sender {
            self.agents[index].messages_sent += 1;
            self.refresh_agent_entry(index);
        }

        // A broadcast reaches every active teammate but the sender, matching
        // how the runtime fans it out.
        let recipients = if to == "*" {
            self.agents
                .iter()
                .enumerate()
                .filter(|(index, row)| {
                    row.origin == AgentOrigin::TeamMember && row.is_active() && Some(*index) != sender
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        } else {
            self.team_row_index(to).into_iter().collect()
        };
        for index in recipients {
            self.agents[index].messages_received += 1;
            self.refresh_agent_entry(index);
        }
    }

    /// Resolve a team address to a row.
    ///
    /// Senders arrive as `name@team` and recipients as a bare `name`, so both
    /// spellings have to land on the same row. The leader has no row of its own
    /// and simply misses.
    fn team_row_index(&self, address: &str) -> Option<usize> {
        if let Some(index) = self.agent_index.get(address).copied() {
            return Some(index);
        }
        let name = address.split_once('@').map_or(address, |(name, _)| name);
        self.agents.iter().position(|row| {
            row.origin == AgentOrigin::TeamMember
                && row.id.split_once('@').is_some_and(|(row_name, _)| row_name == name)
        })
    }

    fn refresh_agent_entry(&mut self, index: usize) {
        let detail = self.agents[index].detail();
        let status = self.agents[index].step_status();
        let entry = self.agents[index].entry;
        if let Some(entry) = self.transcript.get_mut(entry) {
            entry.text = detail;
            entry.tool_status = Some(status);
        }
    }

    fn agent_row_index(&self, id: &str) -> Option<usize> {
        self.agent_index.get(id).copied()
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
