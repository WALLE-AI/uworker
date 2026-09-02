use std::collections::HashMap;
use std::time::{Duration, Instant};

use agentrs_agent::compact::auto::is_compact_boundary;
use agentrs_types::message::{ContentBlock, Message, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EntryKind {
    User,
    Assistant,
    Thinking,
    Tool,
    Info,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolStepStatus {
    Queued,
    Approval,
    Running,
    Success,
    Error,
    Cancelled,
}

impl ToolStepStatus {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Approval => "approval",
            Self::Running => "running",
            Self::Success => "done",
            Self::Error => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn is_terminal(self) -> bool {
        matches!(self, Self::Success | Self::Error | Self::Cancelled)
    }
}

#[derive(Debug)]
pub(super) struct TranscriptEntry {
    pub(super) kind: EntryKind,
    pub(super) label: String,
    pub(super) text: String,
    pub(super) tool_status: Option<ToolStepStatus>,
    display_offset: usize,
    /// Set on the synthetic entries used to flush a partially streamed block
    /// into scrollback. Their header was already printed by an earlier flush.
    continuation: bool,
    /// When this block started streaming. `None` for entries rebuilt from a
    /// stored session, where the original timing is not recoverable.
    started_at: Option<Instant>,
    /// How long the block streamed for, set when it stops being the active
    /// block. `Some` also means "finished", which is what lets a reasoning
    /// block collapse.
    elapsed: Option<Duration>,
}

impl TranscriptEntry {
    pub(super) fn new(kind: EntryKind, label: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            kind,
            label: label.into(),
            text: text.into(),
            tool_status: None,
            display_offset: 0,
            continuation: false,
            started_at: None,
            elapsed: None,
        }
    }

    pub(super) fn tool(label: impl Into<String>, text: impl Into<String>, status: ToolStepStatus) -> Self {
        Self {
            kind: EntryKind::Tool,
            label: label.into(),
            text: text.into(),
            tool_status: Some(status),
            display_offset: 0,
            continuation: false,
            started_at: None,
            elapsed: None,
        }
    }

    /// Start the stream clock. Called when a live block is opened, so a
    /// finished block can report how long the model spent on it.
    pub(super) fn start_clock(&mut self) {
        self.started_at = Some(Instant::now());
    }

    /// Close the block, recording how long it streamed.
    pub(super) fn finish(&mut self) {
        if self.elapsed.is_none() {
            self.elapsed = Some(self.started_at.map_or(Duration::ZERO, |start| start.elapsed()));
        }
    }

    /// Whether the block has stopped streaming.
    pub(super) fn is_finished(&self) -> bool {
        self.elapsed.is_some()
    }

    /// Streaming duration, once the block is finished and was timed.
    pub(super) fn elapsed(&self) -> Option<Duration> {
        self.elapsed.filter(|_| self.started_at.is_some())
    }

    /// Lines the full body occupies, used by the collapsed summary.
    pub(super) fn body_line_count(&self) -> usize {
        self.text.lines().count()
    }

    /// Mark this entry as the tail of a block whose header is already on
    /// screen, so rendering it does not repeat the header.
    pub(super) fn into_continuation(mut self) -> Self {
        self.continuation = true;
        self
    }

    /// Whether this entry should print its own header.
    ///
    /// A block that has already flushed part of itself to scrollback keeps
    /// reading as one block instead of restarting on every stream chunk.
    pub(super) fn shows_label(&self) -> bool {
        !self.continuation && self.display_offset == 0
    }

    pub(super) fn visible_text(&self) -> &str {
        &self.text[self.display_offset.min(self.text.len())..]
    }

    pub(super) fn advance_display_offset(&mut self, byte_count: usize) {
        self.display_offset = self.display_offset.saturating_add(byte_count).min(self.text.len());
    }

    pub(super) fn reset_display_offset(&mut self) {
        self.display_offset = 0;
    }

    pub(super) fn is_stable_for_history(&self) -> bool {
        self.kind != EntryKind::Tool || self.tool_status.is_some_and(ToolStepStatus::is_terminal)
    }
}

pub(super) fn entries_from_messages(messages: &[Message]) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    let mut tool_entries = HashMap::new();
    let mut skip_compact_summary = false;
    for message in messages {
        if skip_compact_summary {
            skip_compact_summary = false;
            continue;
        }
        if is_compact_boundary(message) {
            skip_compact_summary = true;
            continue;
        }
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => match message.role {
                    Role::User => entries.push(TranscriptEntry::new(EntryKind::User, "", text)),
                    Role::Assistant => entries.push(TranscriptEntry::new(EntryKind::Assistant, "", text)),
                    Role::System => entries.push(TranscriptEntry::new(EntryKind::Info, "System", text)),
                    Role::Tool => entries.push(TranscriptEntry::tool("Tool", text, ToolStepStatus::Success)),
                },
                ContentBlock::Thinking { thinking, .. } => {
                    // Replayed history is finished by definition, so it can
                    // collapse straight away; the original timing is lost.
                    let mut entry = TranscriptEntry::new(EntryKind::Thinking, "Thinking", thinking);
                    entry.finish();
                    entries.push(entry);
                }
                ContentBlock::ToolUse { id, name, input, .. } => {
                    entries.push(TranscriptEntry::tool(name, input.to_string(), ToolStepStatus::Queued));
                    tool_entries.insert(id.clone(), entries.len() - 1);
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let status = if *is_error {
                        ToolStepStatus::Error
                    } else {
                        ToolStepStatus::Success
                    };
                    if let Some(index) = tool_entries.get(tool_use_id).copied() {
                        entries[index].text = content.clone();
                        entries[index].tool_status = Some(status);
                    } else {
                        entries.push(TranscriptEntry::tool("Tool result", content, status));
                    }
                }
                ContentBlock::Image { .. } => {
                    entries.push(TranscriptEntry::new(EntryKind::Info, "Image", "[attached image]"));
                }
                ContentBlock::ProviderItem { .. } => {}
            }
        }
    }
    entries
}
